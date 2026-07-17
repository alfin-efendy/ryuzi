//! User-defined custom providers. Persisted in `settings` and registered as
//! leaked `&'static` descriptors so the router resolves them like built-ins.
//! Each custom provider is its own family head (`family == id`), ApiKey
//! category, base-URL-required; the wire format is chosen at Add Account.

use serde::{Deserialize, Serialize};
use specta::Type;

use crate::llm_router::registry::{
    self, ApiFormat, AuthScheme, ProviderCategory, ProviderDescriptor, ProviderToolTransport,
};
use crate::llm_router::{connections, installed};
use crate::store::Store;

const SETTING_KEY: &str = "custom_providers";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct CustomProvider {
    pub id: String,
    pub name: String,
    /// "openai" | "anthropic" — the wire format the endpoint speaks.
    pub format: String,
    pub color: String,
    pub initial: String,
    pub created_at: i64,
}

fn slug(name: &str) -> String {
    let base: String = name
        .to_lowercase()
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
        .collect();
    let squeezed = base
        .split('-')
        .filter(|s| !s.is_empty())
        .collect::<Vec<_>>()
        .join("-");
    let body = if squeezed.is_empty() {
        "provider"
    } else {
        squeezed.as_str()
    };
    format!("custom-{body}")
}

pub async fn list_custom_providers(store: &Store) -> anyhow::Result<Vec<CustomProvider>> {
    let Some(raw) = store.get_setting_raw(SETTING_KEY).await? else {
        return Ok(Vec::new());
    };
    Ok(serde_json::from_str(&raw).unwrap_or_default())
}

async fn persist(store: &Store, providers: &[CustomProvider]) -> anyhow::Result<()> {
    store
        .set_setting_raw(SETTING_KEY, &serde_json::to_string(providers)?)
        .await
}

/// Leak a `CustomProvider` into a `&'static ProviderDescriptor` and register it.
///
/// The leak is intentional and bounded: one small allocation per custom
/// provider, held for the process lifetime, so the descriptor can be returned
/// as `&'static` from the router's shared `descriptor` primitive just like a
/// built-in. Re-registering an existing id (e.g. on a format change) leaks a
/// fresh descriptor and replaces the cache entry; the old allocation is not
/// reclaimed but is unreachable.
pub fn register(cp: &CustomProvider) {
    let format = if cp.format == "anthropic" {
        ApiFormat::Anthropic
    } else {
        ApiFormat::OpenAi
    };
    let auth = match format {
        ApiFormat::Anthropic => AuthScheme::XApiKey,
        ApiFormat::OpenAi => AuthScheme::Bearer,
    };
    let id: &'static str = Box::leak(cp.id.clone().into_boxed_str());
    let desc: &'static ProviderDescriptor = Box::leak(Box::new(ProviderDescriptor {
        id,
        name: Box::leak(cp.name.clone().into_boxed_str()),
        family: id,
        color: Box::leak(cp.color.clone().into_boxed_str()),
        initial: Box::leak(cp.initial.clone().into_boxed_str()),
        category: ProviderCategory::ApiKey,
        format,
        tool_transport: ProviderToolTransport::for_format(format),
        base_url: None,
        auth,
        models: &[],
        requires_base_url: true,
        oauth: None,
        no_auth: false,
        device_flow: None,
        free_tier: false,
        risk_notice: false,
        chat_path: None,
        has_models_endpoint: true,
        uses_max_completion_tokens: false,
        device_grant: None,
    }));
    registry::register_custom_descriptor(desc);
}

pub fn unregister(id: &str) {
    registry::unregister_custom_descriptor(id);
}

/// Register every persisted custom provider (call once at daemon boot).
pub async fn load_and_register_all(store: &Store) -> anyhow::Result<()> {
    for cp in list_custom_providers(store).await? {
        register(&cp);
    }
    Ok(())
}

pub async fn add_custom_provider(store: &Store, name: &str) -> anyhow::Result<Vec<CustomProvider>> {
    let name = name.trim();
    anyhow::ensure!(!name.is_empty(), "provider name is required");
    let mut providers = list_custom_providers(store).await?;
    let mut id = slug(name);
    let mut n = 2;
    while providers.iter().any(|p| p.id == id) || registry::descriptor(&id).is_some() {
        id = format!("{}-{n}", slug(name));
        n += 1;
    }
    let cp = CustomProvider {
        id: id.clone(),
        name: name.to_string(),
        format: "openai".into(),
        color: "#8B8B8B".into(),
        initial: name
            .chars()
            .next()
            .unwrap_or('C')
            .to_uppercase()
            .to_string(),
        created_at: crate::paths::now_ms(),
    };
    providers.push(cp.clone());
    persist(store, &providers).await?;
    // Register the descriptor BEFORE installing: `install_provider` validates
    // `family_of(id) == Some(id)`, which only holds once the descriptor is in
    // the cache (custom providers are their own family head, `family == id`).
    register(&cp);
    installed::install_provider(store, &cp.id).await?;
    Ok(providers)
}

pub async fn set_custom_provider_format(
    store: &Store,
    id: &str,
    format: &str,
) -> anyhow::Result<Vec<CustomProvider>> {
    anyhow::ensure!(
        format == "openai" || format == "anthropic",
        "invalid format: {format}"
    );
    let mut providers = list_custom_providers(store).await?;
    let Some(cp) = providers.iter_mut().find(|p| p.id == id) else {
        anyhow::bail!("unknown custom provider: {id}");
    };
    cp.format = format.to_string();
    let updated = cp.clone();
    persist(store, &providers).await?;
    register(&updated); // re-register with the new format/auth
    Ok(providers)
}

pub async fn remove_custom_provider(
    store: &Store,
    id: &str,
) -> anyhow::Result<Vec<CustomProvider>> {
    let mut providers = list_custom_providers(store).await?;
    providers.retain(|p| p.id != id);
    persist(store, &providers).await?;
    unregister(id);
    installed::uninstall_provider(store, id).await?;
    // Delete the provider's connection rows too, so removing a custom provider
    // never leaves orphaned credentials behind. A custom provider is its own
    // family head (family == id) with no sub-members, so every connection it
    // owns carries `provider == id`.
    for conn in connections::list_connections(store).await? {
        if conn.provider == id {
            connections::remove_connection(store, &conn.id).await?;
        }
    }
    Ok(providers)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn add_registers_a_resolvable_family_head() {
        let db = tempfile::NamedTempFile::new().unwrap();
        let store = Store::open(db.path()).await.unwrap();
        let list = add_custom_provider(&store, "My LiteLLM").await.unwrap();
        let cp = &list[0];
        assert_eq!(cp.id, "custom-my-litellm");
        // Resolvable via the shared descriptor primitive...
        let desc = registry::descriptor(&cp.id).expect("custom descriptor");
        assert_eq!(desc.family, cp.id);
        assert!(desc.requires_base_url);
        // ...and its own family head (so routes accept it).
        assert_eq!(registry::family_of(&cp.id), Some(desc.id));
        // Auto-installed.
        assert!(installed::is_installed(
            &installed::list_installed_providers(&store).await.unwrap(),
            &cp.id
        ));
    }

    #[tokio::test]
    async fn set_format_switches_wire_and_auth() {
        let db = tempfile::NamedTempFile::new().unwrap();
        let store = Store::open(db.path()).await.unwrap();
        let list = add_custom_provider(&store, "Gate Format").await.unwrap();
        let id = list[0].id.clone();
        assert!(matches!(
            registry::descriptor(&id).unwrap().format,
            ApiFormat::OpenAi
        ));
        set_custom_provider_format(&store, &id, "anthropic")
            .await
            .unwrap();
        assert!(matches!(
            registry::descriptor(&id).unwrap().format,
            ApiFormat::Anthropic
        ));
        assert!(matches!(
            registry::descriptor(&id).unwrap().auth,
            AuthScheme::XApiKey
        ));
    }

    #[tokio::test]
    async fn runtime_custom_descriptors_derive_conservative_tools_from_wire_format() {
        let db = tempfile::NamedTempFile::new().unwrap();
        let store = Store::open(db.path()).await.unwrap();
        let list = add_custom_provider(&store, "Transport Facts")
            .await
            .unwrap();
        let id = list[0].id.clone();

        let openai = registry::descriptor(&id)
            .unwrap()
            .tool_transport
            .capabilities();
        assert_eq!(
            openai.wire_protocol,
            crate::harness::native::capabilities::WireProtocol::OpenAiChat
        );
        assert!(openai.supports_function_tools);
        assert!(!openai.supports_strict_function_schema);
        assert!(!openai.supports_custom_freeform_tools);

        set_custom_provider_format(&store, &id, "anthropic")
            .await
            .unwrap();
        let anthropic = registry::descriptor(&id)
            .unwrap()
            .tool_transport
            .capabilities();
        assert_eq!(
            anthropic.wire_protocol,
            crate::harness::native::capabilities::WireProtocol::AnthropicMessages
        );
        assert!(anthropic.supports_function_tools);
        assert!(!anthropic.supports_strict_function_schema);
        assert!(!anthropic.supports_custom_freeform_tools);
    }

    #[tokio::test]
    async fn remove_unregisters_and_uninstalls() {
        let db = tempfile::NamedTempFile::new().unwrap();
        let store = Store::open(db.path()).await.unwrap();
        let list = add_custom_provider(&store, "Temp").await.unwrap();
        let id = list[0].id.clone();
        remove_custom_provider(&store, &id).await.unwrap();
        assert!(registry::descriptor(&id).is_none());
        assert!(list_custom_providers(&store).await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn remove_deletes_the_providers_connection_rows() {
        let db = tempfile::NamedTempFile::new().unwrap();
        let store = Store::open(db.path()).await.unwrap();
        let list = add_custom_provider(&store, "Gate Remove").await.unwrap();
        let id = list[0].id.clone();
        // Attach a connection to the custom provider.
        connections::add_connection(
            &store,
            connections::ConnectionRow {
                id: crate::paths::new_id(),
                provider: id.clone(),
                auth_type: "api_key".into(),
                label: "Gate key".into(),
                priority: 0,
                enabled: true,
                data: connections::ConnectionData::default(),
                created_at: 0,
                updated_at: 0,
            },
        )
        .await
        .unwrap();
        assert!(
            connections::list_connections(&store)
                .await
                .unwrap()
                .iter()
                .any(|c| c.provider == id),
            "connection was created"
        );

        remove_custom_provider(&store, &id).await.unwrap();

        // Descriptor gone AND no orphaned connection rows survive.
        assert!(registry::descriptor(&id).is_none());
        assert!(
            connections::list_connections(&store)
                .await
                .unwrap()
                .iter()
                .all(|c| c.provider != id),
            "removing a custom provider deletes its connection rows"
        );
    }
}
