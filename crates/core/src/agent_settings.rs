//! Native agent settings: the default model and permission mode for the
//! in-process Ryuzi harness, persisted as plain settings-KV rows
//! (`agent_model` / `agent_perm_mode`). Replaces the per-runtime config
//! that used to live in the `agents` table; the store migration that
//! copies old values across lands with the engine deletions (commit 3).

use crate::store::Store;

/// Settings-KV key holding the agent's default model (same value format the
/// `agents` table's `model` column used, e.g. "anthropic/claude-opus-4-5"
/// or a route alias like "free"). Absent = router default.
pub const KEY_MODEL: &str = "agent_model";
/// Settings-KV key holding the agent's permission mode:
/// "plan" | "ask" | "edit" | "full". Absent = engine default ("ask").
pub const KEY_PERM_MODE: &str = "agent_perm_mode";

#[derive(Debug, Clone, Default, PartialEq)]
pub struct AgentSettings {
    pub model: Option<String>,
    pub perm_mode: Option<String>,
}

/// Read both keys; a missing key surfaces as `None` (never an error).
pub async fn get(store: &Store) -> anyhow::Result<AgentSettings> {
    Ok(AgentSettings {
        model: store.get_setting(KEY_MODEL).await?,
        perm_mode: store.get_setting(KEY_PERM_MODE).await?,
    })
}

/// Persist the settings: `Some(v)` upserts the key, `None` deletes it, so a
/// cleared field falls back to the engine default instead of pinning "".
/// Retained temporarily for the session lifecycle fallback until profile
/// snapshots replace it. Writes remain attributed to `WriteOrigin::User`.
pub async fn set(store: &Store, s: &AgentSettings) -> anyhow::Result<()> {
    match &s.model {
        Some(v) => {
            store
                .set_setting(crate::domain::WriteOrigin::User, KEY_MODEL, v)
                .await?
        }
        None => store.delete_setting_raw(KEY_MODEL).await?,
    }
    match &s.perm_mode {
        Some(v) => {
            store
                .set_setting(crate::domain::WriteOrigin::User, KEY_PERM_MODE, v)
                .await?
        }
        None => store.delete_setting_raw(KEY_PERM_MODE).await?,
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::Store;

    #[tokio::test]
    async fn get_on_a_fresh_store_returns_all_none() {
        let tmp = tempfile::NamedTempFile::new().unwrap();
        let store = Store::open(tmp.path()).await.unwrap();
        assert_eq!(get(&store).await.unwrap(), AgentSettings::default());
    }

    #[tokio::test]
    async fn set_then_get_round_trips_both_fields() {
        let tmp = tempfile::NamedTempFile::new().unwrap();
        let store = Store::open(tmp.path()).await.unwrap();
        let s = AgentSettings {
            model: Some("anthropic/claude-opus-4-5".into()),
            perm_mode: Some("edit".into()),
        };
        set(&store, &s).await.unwrap();
        assert_eq!(get(&store).await.unwrap(), s);
        // Pinned key names — the group-3 migration writes these exact rows.
        assert_eq!(
            store.get_setting("agent_model").await.unwrap().as_deref(),
            Some("anthropic/claude-opus-4-5")
        );
        assert_eq!(
            store
                .get_setting("agent_perm_mode")
                .await
                .unwrap()
                .as_deref(),
            Some("edit")
        );
    }

    #[tokio::test]
    async fn set_none_deletes_a_previously_written_key() {
        let tmp = tempfile::NamedTempFile::new().unwrap();
        let store = Store::open(tmp.path()).await.unwrap();
        set(
            &store,
            &AgentSettings {
                model: Some("free".into()),
                perm_mode: Some("full".into()),
            },
        )
        .await
        .unwrap();
        // Clearing the model keeps the perm mode and deletes the model row.
        set(
            &store,
            &AgentSettings {
                model: None,
                perm_mode: Some("full".into()),
            },
        )
        .await
        .unwrap();
        assert_eq!(store.get_setting(KEY_MODEL).await.unwrap(), None);
        assert_eq!(
            get(&store).await.unwrap(),
            AgentSettings {
                model: None,
                perm_mode: Some("full".into())
            }
        );
    }
}
