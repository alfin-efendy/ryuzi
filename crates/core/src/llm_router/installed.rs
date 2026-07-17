//! Persisted set of "installed" provider families. The Cockpit Models list
//! shows only installed providers; install/uninstall toggles membership. This
//! is deliberately distinct from having a connection — a provider can be
//! installed with zero accounts (you install it, then add an account).

use crate::llm_router::{connections, registry};
use crate::store::Store;

const SETTING_KEY: &str = "installed_providers";
const SEEDED_MARKER: &str = "installed_providers_seeded_v1";

/// Providers installed by default on a fresh install.
const DEFAULT_INSTALLED: &[&str] = &[
    "anthropic",
    "openai",
    "mimo-free",
    "opencode-free",
    "google",
    "openrouter",
    "groq",
    "ollama",
];

pub async fn list_installed_providers(store: &Store) -> anyhow::Result<Vec<String>> {
    let Some(raw) = store.get_setting_raw(SETTING_KEY).await? else {
        return Ok(Vec::new());
    };
    Ok(serde_json::from_str(&raw).unwrap_or_default())
}

async fn persist(store: &Store, families: &[String]) -> anyhow::Result<()> {
    store
        .set_setting_raw(SETTING_KEY, &serde_json::to_string(families)?)
        .await
}

pub fn is_installed(installed: &[String], family: &str) -> bool {
    installed.iter().any(|f| f == family)
}

/// Add `family` to the installed set (idempotent). `family` must be a registry
/// family head. Returns the new set.
pub async fn install_provider(store: &Store, family: &str) -> anyhow::Result<Vec<String>> {
    if registry::family_of(family) != Some(family) {
        anyhow::bail!("unknown provider family: {family}");
    }
    let mut set = list_installed_providers(store).await?;
    if !is_installed(&set, family) {
        set.push(family.to_string());
        persist(store, &set).await?;
    }
    Ok(set)
}

/// Remove `family` from the installed set (idempotent). Existing connections
/// are left untouched. Returns the new set.
pub async fn uninstall_provider(store: &Store, family: &str) -> anyhow::Result<Vec<String>> {
    let mut set = list_installed_providers(store).await?;
    set.retain(|f| f != family);
    persist(store, &set).await?;
    Ok(set)
}

/// Seed the installed set once: the default list plus every family that already
/// has a connection (so upgrading users keep their configured providers).
pub async fn ensure_default_installed_providers(store: &Store) -> anyhow::Result<()> {
    if store.get_setting_raw(SEEDED_MARKER).await?.is_some() {
        return Ok(());
    }
    let mut set: Vec<String> = DEFAULT_INSTALLED.iter().map(|s| s.to_string()).collect();
    for conn in connections::list_connections(store).await? {
        if let Some(family) = registry::family_of(&conn.provider) {
            if !is_installed(&set, family) {
                set.push(family.to_string());
            }
        }
    }
    persist(store, &set).await?;
    store.set_setting_raw(SEEDED_MARKER, "1").await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn install_and_uninstall_are_idempotent() {
        let db = tempfile::NamedTempFile::new().unwrap();
        let store = Store::open(db.path()).await.unwrap();

        let set = install_provider(&store, "anthropic").await.unwrap();
        assert!(is_installed(&set, "anthropic"));
        // Second install is a no-op (no duplicate).
        let set = install_provider(&store, "anthropic").await.unwrap();
        assert_eq!(set.iter().filter(|f| *f == "anthropic").count(), 1);

        let set = uninstall_provider(&store, "anthropic").await.unwrap();
        assert!(!is_installed(&set, "anthropic"));
        // Uninstalling something absent is fine.
        uninstall_provider(&store, "anthropic").await.unwrap();
    }

    #[tokio::test]
    async fn install_rejects_non_family_head() {
        let db = tempfile::NamedTempFile::new().unwrap();
        let store = Store::open(db.path()).await.unwrap();
        // `anthropic-oauth` is a member, not a family head.
        assert!(install_provider(&store, "anthropic-oauth").await.is_err());
        assert!(install_provider(&store, "does-not-exist").await.is_err());
    }

    #[tokio::test]
    async fn seed_unions_defaults_with_existing_connection_families() {
        let db = tempfile::NamedTempFile::new().unwrap();
        let store = Store::open(db.path()).await.unwrap();
        connections::add_connection(
            &store,
            connections::ConnectionRow {
                id: crate::paths::new_id(),
                provider: "kiro".into(),
                auth_type: "device".into(),
                label: "Kiro".into(),
                priority: 0,
                enabled: true,
                data: connections::ConnectionData::default(),
                created_at: 0,
                updated_at: 0,
            },
        )
        .await
        .unwrap();

        ensure_default_installed_providers(&store).await.unwrap();
        let set = list_installed_providers(&store).await.unwrap();
        assert!(is_installed(&set, "anthropic")); // default
        assert!(is_installed(&set, "mimo-free")); // default
        assert!(is_installed(&set, "kiro")); // migrated from an existing connection

        // Idempotent: seeding again does not change the set.
        let before = set.len();
        ensure_default_installed_providers(&store).await.unwrap();
        assert_eq!(
            list_installed_providers(&store).await.unwrap().len(),
            before
        );
    }
}
