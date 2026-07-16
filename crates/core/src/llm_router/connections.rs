//! Provider connections: a provider + credential + priority row the router
//! can route requests through. Secrets live in the `data` JSON blob, encrypted
//! at rest field-by-field via [`crate::llm_router::secrets`] (decrypted
//! transparently on read).
use crate::llm_router::registry::{ProviderDescriptor, ProviderEndpointSource};
use crate::llm_router::secrets;
use crate::store::Store;
use rusqlite::{params, OptionalExtension, Row};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct ConnectionData {
    pub api_key: Option<String>,
    pub base_url_override: Option<String>,
    pub models_override: Option<Vec<String>>,
    pub model_meta_overrides:
        Option<HashMap<String, crate::llm_router::model_effort::DiscoveredModelMeta>>,
    // OAuth (auth_type == "oauth"): tokens are encrypted at rest via
    // secrets::SecretCipher (keychain-derived master key, file fallback when
    // the keychain is unavailable) and decrypted transparently on read.
    pub access_token: Option<String>,
    pub refresh_token: Option<String>,
    pub expires_at: Option<i64>,
    pub last_refresh_at: Option<i64>,
    pub provider_specific: Option<serde_json::Value>,
    pub needs_relogin: Option<bool>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ConnectionRow {
    pub id: String,
    pub provider: String,
    pub auth_type: String,
    pub label: String,
    pub priority: i64,
    pub enabled: bool,
    pub data: ConnectionData,
    pub created_at: i64,
    pub updated_at: i64,
}

const COLS: &str = "id,provider,auth_type,label,priority,enabled,data,created_at,updated_at";

fn row_to_conn(r: &Row) -> rusqlite::Result<ConnectionRow> {
    let raw: String = r.get(6)?;
    let mut data: ConnectionData = serde_json::from_str(&raw).unwrap_or_default();
    secrets::decrypt_conn_data(&mut data);
    Ok(ConnectionRow {
        id: r.get(0)?,
        provider: r.get(1)?,
        auth_type: r.get(2)?,
        label: r.get(3)?,
        priority: r.get(4)?,
        enabled: r.get::<_, i64>(5)? != 0,
        data,
        created_at: r.get(7)?,
        updated_at: r.get(8)?,
    })
}

pub async fn list_connections(store: &Store) -> anyhow::Result<Vec<ConnectionRow>> {
    store
        .with_conn(|c| -> rusqlite::Result<Vec<ConnectionRow>> {
            let mut stmt = c.prepare(&format!(
                "SELECT {COLS} FROM provider_connections ORDER BY priority ASC, created_at ASC"
            ))?;
            let items = stmt
                .query_map([], row_to_conn)?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            Ok(items)
        })
        .await
}

pub async fn get_connection(store: &Store, id: &str) -> anyhow::Result<Option<ConnectionRow>> {
    let id = id.to_string();
    store
        .with_conn(move |c| {
            c.query_row(
                &format!("SELECT {COLS} FROM provider_connections WHERE id=?1"),
                params![id],
                row_to_conn,
            )
            .optional()
        })
        .await
}

/// Insert a connection. `row.priority` is IGNORED — the new row is always
/// appended at the end (MAX(priority)+1); reorder with [`move_connection`].
pub async fn add_connection(store: &Store, row: ConnectionRow) -> anyhow::Result<()> {
    let mut d = row.data.clone();
    secrets::encrypt_conn_data(&mut d);
    let data = serde_json::to_string(&d)?;
    store
        .with_conn(move |c| {
            c.execute(
                "INSERT INTO provider_connections(id,provider,auth_type,label,priority,enabled,data,created_at,updated_at) \
                 VALUES (?1,?2,?3,?4,\
                   COALESCE((SELECT MAX(priority)+1 FROM provider_connections), 0),\
                   ?5,?6,?7,?8)",
                params![row.id, row.provider, row.auth_type, row.label,
                        row.enabled as i64, data, row.created_at, row.updated_at],
            )
            .map(|_| ())
        })
        .await
}

/// Update the mutable fields of a connection: `label`, `enabled`, and
/// `data` (+ `updated_at`). `provider`, `auth_type`, and `priority` are
/// NOT written — identity is fixed and ordering changes go through
/// [`move_connection`].
pub async fn update_connection(store: &Store, row: ConnectionRow) -> anyhow::Result<()> {
    let mut d = row.data.clone();
    secrets::encrypt_conn_data(&mut d);
    let data = serde_json::to_string(&d)?;
    store
        .with_conn(move |c| {
            c.execute(
                "UPDATE provider_connections SET label=?2, enabled=?3, data=?4, updated_at=?5 WHERE id=?1",
                params![row.id, row.label, row.enabled as i64, data, row.updated_at],
            )
            .map(|_| ())
        })
        .await
}

pub async fn rename_connection(store: &Store, id: &str, label: &str) -> anyhow::Result<()> {
    let unknown_id = id.to_string();
    let id = unknown_id.clone();
    let label = label.to_string();
    let updated_at = crate::paths::now_ms();
    let affected = store
        .with_conn(move |c| {
            c.execute(
                "UPDATE provider_connections SET label=?2, updated_at=?3 WHERE id=?1",
                params![id, label, updated_at],
            )
        })
        .await?;
    if affected == 0 {
        return Err(anyhow::anyhow!("unknown connection: {unknown_id}"));
    }
    Ok(())
}

pub async fn set_connection_enabled(store: &Store, id: &str, enabled: bool) -> anyhow::Result<()> {
    let unknown_id = id.to_string();
    let id = unknown_id.clone();
    let updated_at = crate::paths::now_ms();
    let affected = store
        .with_conn(move |c| {
            c.execute(
                "UPDATE provider_connections SET enabled=?2, updated_at=?3 WHERE id=?1",
                params![id, enabled as i64, updated_at],
            )
        })
        .await?;
    if affected == 0 {
        return Err(anyhow::anyhow!("unknown connection: {unknown_id}"));
    }
    Ok(())
}

pub async fn remove_connection(store: &Store, id: &str) -> anyhow::Result<()> {
    let id = id.to_string();
    store
        .with_conn(move |c| {
            c.execute("DELETE FROM provider_connections WHERE id=?1", params![id])
                .map(|_| ())
        })
        .await
}

/// Swap priority with the neighbor above (`dir=-1`) or below (`dir=1`).
pub async fn move_connection(store: &Store, id: &str, dir: i32) -> anyhow::Result<()> {
    let id = id.to_string();
    store
        .with_conn(move |c| {
            let tx = c.transaction()?;
            // Normalize priorities to 0..n first so swaps are well-defined.
            let ids: Vec<String> = {
                let mut stmt = tx.prepare(
                    "SELECT id FROM provider_connections ORDER BY priority ASC, created_at ASC",
                )?;
                let v = stmt
                    .query_map([], |r| r.get::<_, String>(0))?
                    .collect::<rusqlite::Result<Vec<_>>>()?;
                v
            };
            for (i, cid) in ids.iter().enumerate() {
                tx.execute(
                    "UPDATE provider_connections SET priority=?2 WHERE id=?1",
                    params![cid, i as i64],
                )?;
            }
            // Neighbor = adjacent connection of the SAME FAMILY; rows from
            // other families keep their slots (their priorities are swapped
            // *values*, not shifted).
            let providers: Vec<String> = {
                let mut stmt = tx.prepare(
                    "SELECT provider FROM provider_connections ORDER BY priority ASC, created_at ASC",
                )?;
                let v = stmt
                    .query_map([], |r| r.get::<_, String>(0))?
                    .collect::<rusqlite::Result<Vec<_>>>()?;
                v
            };
            let family_at = |i: usize| {
                crate::llm_router::registry::family_of(&providers[i])
                    .unwrap_or(providers[i].as_str())
                    .to_string()
            };
            if let Some(pos) = ids.iter().position(|c2| *c2 == id) {
                let fam = family_at(pos);
                let mut other: Option<usize> = None;
                let mut i = pos as i64 + dir as i64;
                while i >= 0 && (i as usize) < ids.len() {
                    if family_at(i as usize) == fam {
                        other = Some(i as usize);
                        break;
                    }
                    i += dir as i64;
                }
                if let Some(other) = other {
                    tx.execute(
                        "UPDATE provider_connections SET priority=?2 WHERE id=?1",
                        params![ids[pos], other as i64],
                    )?;
                    tx.execute(
                        "UPDATE provider_connections SET priority=?2 WHERE id=?1",
                        params![ids[other], pos as i64],
                    )?;
                }
            }
            tx.commit()?;
            Ok(())
        })
        .await
}

pub fn is_oauth(row: &ConnectionRow) -> bool {
    row.auth_type == "oauth"
}

/// Kiro `provider_specific` accessors. The blob shape mirrors 9router:
/// { authMethod, profileArn?, region?, clientId?, clientSecret? }.
/// Ported from 9router (MIT, (c) 2024-2026 decolua and contributors).
pub fn kiro_auth_method(d: &ConnectionData) -> String {
    d.provider_specific
        .as_ref()
        .and_then(|v| v.get("authMethod"))
        .and_then(|v| v.as_str())
        .unwrap_or("builder-id")
        .to_string()
}

pub fn kiro_profile_arn(d: &ConnectionData) -> Option<String> {
    d.provider_specific
        .as_ref()
        .and_then(|v| v.get("profileArn"))
        .and_then(|v| v.as_str())
        .filter(|s| !s.trim().is_empty())
        .map(|s| s.to_string())
}

pub fn kiro_region(d: &ConnectionData) -> String {
    d.provider_specific
        .as_ref()
        .and_then(|v| v.get("region"))
        .and_then(|v| v.as_str())
        .filter(|s| !s.trim().is_empty())
        .unwrap_or("us-east-1")
        .to_string()
}

pub fn kiro_client_creds(d: &ConnectionData) -> Option<(String, String)> {
    let ps = d.provider_specific.as_ref()?;
    let id = ps.get("clientId")?.as_str()?.to_string();
    let secret = ps.get("clientSecret")?.as_str()?.to_string();
    Some((id, secret))
}

pub fn is_account_bound(auth_method: &str) -> bool {
    matches!(auth_method, "api_key" | "idc" | "external_idp")
}

/// Shared default CodeWhisperer profile ARN for non-account-bound auth.
pub fn default_profile_arn(auth_method: &str) -> &'static str {
    match auth_method {
        "google" | "github" => "arn:aws:codewhisperer:us-east-1:699475941385:profile/EHGA3GRVQMUK",
        _ => "arn:aws:codewhisperer:us-east-1:638616132270:profile/AAAACCCCXXXX",
    }
}

pub fn effective_base_url(desc: &ProviderDescriptor, row: &ConnectionRow) -> Option<String> {
    row.data
        .base_url_override
        .clone()
        .or_else(|| desc.base_url.map(|s| s.to_string()))
        .map(|s| s.trim_end_matches('/').to_string())
}

pub fn endpoint_source(row: &ConnectionRow) -> ProviderEndpointSource {
    if row
        .data
        .base_url_override
        .as_deref()
        .is_some_and(|base_url| !base_url.trim().is_empty())
    {
        ProviderEndpointSource::ConnectionOverride
    } else {
        ProviderEndpointSource::Catalog
    }
}

pub fn effective_models(desc: &ProviderDescriptor, row: &ConnectionRow) -> Vec<String> {
    match &row.data.models_override {
        Some(m) if !m.is_empty() => m.clone(),
        _ => desc.models.iter().map(|s| s.to_string()).collect(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::Store;

    async fn mem_store() -> Store {
        let tmp = tempfile::NamedTempFile::new().unwrap();
        // Keep the file alive by leaking the handle for the test's duration.
        let (_, path) = tmp.keep().unwrap();
        Store::open(&path).await.unwrap()
    }

    fn row(id: &str, provider: &str, prio: i64) -> ConnectionRow {
        ConnectionRow {
            id: id.into(),
            provider: provider.into(),
            auth_type: "api_key".into(),
            label: format!("{id} label"),
            priority: prio,
            enabled: true,
            data: ConnectionData {
                api_key: Some("sk-test".into()),
                base_url_override: None,
                models_override: None,
                ..Default::default()
            },
            created_at: 1,
            updated_at: 1,
        }
    }

    #[tokio::test]
    async fn crud_and_priority_ordering() {
        secrets::use_test_key_file();
        let store = mem_store().await;
        add_connection(&store, row("c1", "openai", 0))
            .await
            .unwrap();
        // Same family as c1 ("openai") so the move below has a same-family
        // neighbor to swap with — move_connection is family-scoped (see
        // move_swaps_within_family_skipping_other_providers for the
        // cross-family case).
        add_connection(&store, row("c2", "openai", 1))
            .await
            .unwrap();
        let list = list_connections(&store).await.unwrap();
        assert_eq!(list.len(), 2);
        assert_eq!(list[0].id, "c1"); // ordered by priority ASC
        assert_eq!(list[0].data.api_key.as_deref(), Some("sk-test"));

        // move c2 up → it becomes priority 0
        move_connection(&store, "c2", -1).await.unwrap();
        let list = list_connections(&store).await.unwrap();
        assert_eq!(list[0].id, "c2");

        let mut c1 = get_connection(&store, "c1").await.unwrap().unwrap();
        c1.label = "renamed".into();
        c1.data.models_override = Some(vec!["m1".into()]);
        update_connection(&store, c1).await.unwrap();
        assert_eq!(
            get_connection(&store, "c1").await.unwrap().unwrap().label,
            "renamed"
        );

        remove_connection(&store, "c1").await.unwrap();
        assert_eq!(list_connections(&store).await.unwrap().len(), 1);
    }

    async fn raw_data(store: &Store, id: &str) -> String {
        let id = id.to_string();
        store
            .with_conn(move |c| {
                c.query_row(
                    "SELECT data FROM provider_connections WHERE id=?1",
                    params![id],
                    |r| r.get(0),
                )
            })
            .await
            .unwrap()
    }

    #[tokio::test]
    async fn rename_connection_updates_label_without_rewriting_data_ciphertext() {
        secrets::use_test_key_file();
        let store = mem_store().await;
        add_connection(&store, row("rename", "openai", 0))
            .await
            .unwrap();
        let ciphertext = raw_data(&store, "rename").await;

        rename_connection(&store, "rename", "Renamed account")
            .await
            .unwrap();

        let renamed = get_connection(&store, "rename").await.unwrap().unwrap();
        assert_eq!(renamed.label, "Renamed account");
        assert_eq!(raw_data(&store, "rename").await, ciphertext);
    }

    #[tokio::test]
    async fn set_connection_enabled_updates_flag_without_rewriting_data_ciphertext() {
        secrets::use_test_key_file();
        let store = mem_store().await;
        add_connection(&store, row("enabled", "openai", 0))
            .await
            .unwrap();
        let ciphertext = raw_data(&store, "enabled").await;

        set_connection_enabled(&store, "enabled", false)
            .await
            .unwrap();

        let disabled = get_connection(&store, "enabled").await.unwrap().unwrap();
        assert!(!disabled.enabled);
        assert_eq!(raw_data(&store, "enabled").await, ciphertext);
    }

    #[tokio::test]
    async fn rename_connection_rejects_unknown_id() {
        let store = mem_store().await;
        let err = rename_connection(&store, "missing", "Missing")
            .await
            .unwrap_err();
        assert_eq!(err.to_string(), "unknown connection: missing");
    }

    #[tokio::test]
    async fn set_connection_enabled_rejects_unknown_id() {
        let store = mem_store().await;
        let err = set_connection_enabled(&store, "missing", false)
            .await
            .unwrap_err();
        assert_eq!(err.to_string(), "unknown connection: missing");
    }

    #[tokio::test]
    async fn move_swaps_within_family_skipping_other_providers() {
        // priority order: a (anthropic), x (openai), b (anthropic-oauth)
        // moving b up must swap b with a (same family), leaving x alone.
        // After: order b, x, a  — family list [b, a].
        secrets::use_test_key_file();
        let store = mem_store().await;
        add_connection(&store, row("a", "anthropic", 0))
            .await
            .unwrap();
        add_connection(&store, row("x", "openai", 1)).await.unwrap();
        add_connection(&store, row("b", "anthropic-oauth", 2))
            .await
            .unwrap();

        move_connection(&store, "b", -1).await.unwrap();

        let list = list_connections(&store).await.unwrap();
        let ids: Vec<&str> = list.iter().map(|c| c.id.as_str()).collect();
        assert_eq!(ids, vec!["b", "x", "a"]);
    }

    #[tokio::test]
    async fn effective_helpers_prefer_overrides() {
        let desc = crate::llm_router::registry::descriptor("openai").unwrap();
        let mut r = row("c1", "openai", 0);
        assert_eq!(
            effective_base_url(desc, &r).as_deref(),
            Some("https://api.openai.com/v1")
        );
        assert!(effective_models(desc, &r).contains(&"gpt-5.2".to_string()));
        r.data.base_url_override = Some("http://localhost:9/v1".into());
        r.data.models_override = Some(vec!["custom-model".into()]);
        assert_eq!(
            effective_base_url(desc, &r).as_deref(),
            Some("http://localhost:9/v1")
        );
        assert_eq!(effective_models(desc, &r), vec!["custom-model".to_string()]);
    }

    #[test]
    fn legacy_apikey_data_deserializes_with_new_oauth_fields_absent() {
        let legacy = r#"{"apiKey":"sk-x","baseUrlOverride":null,"modelsOverride":null}"#;
        let d: ConnectionData = serde_json::from_str(legacy).unwrap();
        assert_eq!(d.api_key.as_deref(), Some("sk-x"));
        assert!(d.access_token.is_none() && d.expires_at.is_none());
    }

    #[test]
    fn is_oauth_reads_auth_type() {
        let mut r = row("c1", "anthropic-oauth", 0);
        r.auth_type = "oauth".into();
        assert!(is_oauth(&r));
        r.auth_type = "api_key".into();
        assert!(!is_oauth(&r));
    }

    #[test]
    fn openai_oauth_connection_without_fetched_models_still_serves_seeds() {
        let desc = crate::llm_router::registry::descriptor("openai-oauth").unwrap();
        let row = ConnectionRow {
            id: "c-oauth".into(),
            provider: "openai-oauth".into(),
            auth_type: "oauth".into(),
            label: "ChatGPT".into(),
            priority: 0,
            enabled: true,
            data: ConnectionData::default(), // no models_override yet
            created_at: 0,
            updated_at: 0,
        };
        let models = effective_models(desc, &row);
        assert!(models.contains(&"gpt-5.2-codex".to_string()));
    }

    #[tokio::test]
    async fn at_rest_data_is_ciphertext() {
        secrets::use_test_key_file();
        let store = mem_store().await;
        add_connection(&store, row("c1", "openai", 0))
            .await
            .unwrap();
        let raw: String = store
            .with_conn(|c| {
                c.query_row(
                    "SELECT data FROM provider_connections WHERE id=?1",
                    params!["c1"],
                    |r| r.get::<_, String>(0),
                )
            })
            .await
            .unwrap();
        assert!(
            !raw.contains("sk-test"),
            "raw data must not contain plaintext secret: {raw}"
        );
        assert!(
            raw.contains("enc:v1:"),
            "raw data must contain encrypted marker: {raw}"
        );

        // Decrypt-on-read is transparent: the row still reads back as plaintext.
        let list = list_connections(&store).await.unwrap();
        assert_eq!(list[0].data.api_key.as_deref(), Some("sk-test"));
    }

    #[test]
    fn encrypt_conn_data_is_idempotent() {
        secrets::use_test_key_file();
        let mut d = ConnectionData {
            api_key: Some("sk-test".into()),
            provider_specific: Some(serde_json::json!({"clientSecret": "shh"})),
            ..Default::default()
        };
        secrets::encrypt_conn_data(&mut d);
        let once = d.clone();
        secrets::encrypt_conn_data(&mut d);
        assert_eq!(
            d, once,
            "re-encrypting an already-encrypted row must be a no-op"
        );

        secrets::decrypt_conn_data(&mut d);
        assert_eq!(d.api_key.as_deref(), Some("sk-test"));
        assert_eq!(
            d.provider_specific,
            Some(serde_json::json!({"clientSecret": "shh"}))
        );
    }

    #[test]
    fn kiro_accessors_read_provider_specific() {
        let d = ConnectionData {
            provider_specific: Some(serde_json::json!({
                "authMethod": "idc", "profileArn": "arn:aws:codewhisperer:us-east-1:1:profile/X",
                "region": "eu-west-1", "clientId": "c", "clientSecret": "s"
            })),
            ..Default::default()
        };
        assert_eq!(kiro_auth_method(&d), "idc");
        assert_eq!(kiro_region(&d), "eu-west-1");
        assert_eq!(kiro_client_creds(&d), Some(("c".into(), "s".into())));
        assert!(is_account_bound(&kiro_auth_method(&d)));
        let empty = ConnectionData::default();
        assert_eq!(kiro_auth_method(&empty), "builder-id");
        assert_eq!(kiro_region(&empty), "us-east-1");
        assert!(kiro_profile_arn(&empty).is_none());
        assert_eq!(
            default_profile_arn("github"),
            "arn:aws:codewhisperer:us-east-1:699475941385:profile/EHGA3GRVQMUK"
        );
        assert_eq!(
            default_profile_arn("builder-id"),
            "arn:aws:codewhisperer:us-east-1:638616132270:profile/AAAACCCCXXXX"
        );
    }
}
