//! `ryuzi:storage/storage` host adapter: a plugin's own key/value rows in
//! `component_plugin_storage`, scoped by `plugin_id` at the `Store` layer
//! (`Store::get_component_storage`/`put_component_storage`/
//! `delete_component_storage` all take `plugin_id` as part of their primary
//! key) — a plugin can never see or overwrite another plugin's rows.

use super::PluginCapabilityContext;

/// A capability-adapter-local error, mapped to the generated WIT
/// `storage::StorageError` by the runtime's `Host` trait impl.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StorageErr {
    NotFound,
    Denied,
    Failed(String),
}

/// A plugin's scoped view of its own component storage rows.
pub struct PluginStorage<'a> {
    pub ctx: &'a PluginCapabilityContext,
}

impl<'a> PluginStorage<'a> {
    pub fn new(ctx: &'a PluginCapabilityContext) -> Self {
        Self { ctx }
    }

    pub async fn get(&self, key: &str) -> Result<Vec<u8>, StorageErr> {
        match self
            .ctx
            .store
            .get_component_storage(&self.ctx.plugin_id, key)
            .await
        {
            Ok(Some(record)) => Ok(record.value),
            Ok(None) => Err(StorageErr::NotFound),
            Err(error) => Err(StorageErr::Failed(error.to_string())),
        }
    }

    pub async fn put(&self, key: &str, value: Vec<u8>) -> Result<(), StorageErr> {
        self.ctx
            .store
            .put_component_storage(&self.ctx.plugin_id, key, &value)
            .await
            .map_err(|error| StorageErr::Failed(error.to_string()))
    }

    pub async fn delete(&self, key: &str) -> Result<bool, StorageErr> {
        self.ctx
            .store
            .delete_component_storage(&self.ctx.plugin_id, key)
            .await
            .map_err(|error| StorageErr::Failed(error.to_string()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::settings::SettingsStore;
    use crate::store::{Store, COMPONENT_STORAGE_MAX_VALUE_BYTES};
    use crate::telemetry::NoopTelemetry;
    use std::sync::Arc;

    async fn open_test_store() -> (Arc<Store>, tempfile::NamedTempFile) {
        let tmp = tempfile::NamedTempFile::new().unwrap();
        let store = Store::open(tmp.path()).await.unwrap();
        (Arc::new(store), tmp)
    }

    fn ctx_for(store: Arc<Store>, plugin_id: &str) -> PluginCapabilityContext {
        PluginCapabilityContext {
            plugin_id: plugin_id.to_string(),
            version: "0.1.0".to_string(),
            settings: SettingsStore::new(store.clone()),
            store,
            telemetry: Arc::new(NoopTelemetry),
            network_allowlist: vec![],
            oauth_profile_ids: vec![],
            provider_ids: vec![],
        }
    }

    #[tokio::test]
    async fn storage_is_scoped_independently_per_plugin() {
        let (store, _tmp) = open_test_store().await;
        let github_ctx = ctx_for(store.clone(), "task7s-github");
        let atlassian_ctx = ctx_for(store, "task7s-atlassian");
        let github = PluginStorage::new(&github_ctx);
        let atlassian = PluginStorage::new(&atlassian_ctx);

        github.put("k", b"github-value".to_vec()).await.unwrap();
        atlassian
            .put("k", b"atlassian-value".to_vec())
            .await
            .unwrap();

        assert_eq!(github.get("k").await.unwrap(), b"github-value");
        assert_eq!(atlassian.get("k").await.unwrap(), b"atlassian-value");
    }

    #[tokio::test]
    async fn oversized_value_fails() {
        let (store, _tmp) = open_test_store().await;
        let ctx = ctx_for(store, "task7s-oversized");
        let storage = PluginStorage::new(&ctx);

        let oversized = vec![0u8; COMPONENT_STORAGE_MAX_VALUE_BYTES + 1];
        let result = storage.put("k", oversized).await;
        assert!(
            matches!(result, Err(StorageErr::Failed(_))),
            "expected Failed, got {result:?}"
        );
    }

    #[tokio::test]
    async fn missing_key_is_not_found() {
        let (store, _tmp) = open_test_store().await;
        let ctx = ctx_for(store, "task7s-missing");
        let storage = PluginStorage::new(&ctx);

        let result = storage.get("nope").await;
        assert!(matches!(result, Err(StorageErr::NotFound)));
    }

    #[tokio::test]
    async fn delete_reports_whether_a_row_existed() {
        let (store, _tmp) = open_test_store().await;
        let ctx = ctx_for(store, "task7s-delete");
        let storage = PluginStorage::new(&ctx);

        storage.put("k", b"v".to_vec()).await.unwrap();
        assert!(storage.delete("k").await.unwrap());
        assert!(!storage.delete("k").await.unwrap());
    }
}
