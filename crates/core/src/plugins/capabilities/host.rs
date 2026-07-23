//! `ryuzi:host/host` adapter: plugin identity and granted capability flags.
//! This interface is always linked regardless of policy (see
//! `runtime::instantiate`) — it carries no secrets and has no side effects,
//! it just tells a component what it already knows about itself plus
//! whether network access was granted.

use super::PluginCapabilityContext;

/// A plugin's view of its own identity and granted capabilities.
pub struct HostInfo<'a> {
    pub ctx: &'a PluginCapabilityContext,
    pub allow_network: bool,
}

impl<'a> HostInfo<'a> {
    pub fn new(ctx: &'a PluginCapabilityContext, allow_network: bool) -> Self {
        Self { ctx, allow_network }
    }

    /// `(plugin id, plugin version)`.
    pub fn plugin_info(&self) -> (String, String) {
        (self.ctx.plugin_id.clone(), self.ctx.version.clone())
    }

    /// `(network, filesystem, secrets)`. Filesystem and secrets access are
    /// not granted by any policy this runtime slice supports, so they are
    /// always `false`.
    pub fn capabilities(&self) -> (bool, bool, bool) {
        (self.allow_network, false, false)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::settings::SettingsStore;
    use crate::store::Store;
    use crate::telemetry::NoopTelemetry;
    use std::sync::Arc;

    async fn open_ctx(
        plugin_id: &str,
        version: &str,
    ) -> (PluginCapabilityContext, tempfile::NamedTempFile) {
        let tmp = tempfile::NamedTempFile::new().unwrap();
        let store = Arc::new(Store::open(tmp.path()).await.unwrap());
        (
            PluginCapabilityContext {
                plugin_id: plugin_id.to_string(),
                version: version.to_string(),
                settings: SettingsStore::new(store.clone()),
                store,
                telemetry: Arc::new(NoopTelemetry),
                network_allowlist: vec![],
                oauth_profile_ids: vec![],
                provider_ids: vec![],
            },
            tmp,
        )
    }

    #[tokio::test]
    async fn plugin_info_reflects_the_context() {
        let (ctx, _tmp) = open_ctx("task7-hostinfo", "1.2.3").await;
        let info = HostInfo::new(&ctx, false);
        assert_eq!(
            info.plugin_info(),
            ("task7-hostinfo".to_string(), "1.2.3".to_string())
        );
    }

    #[tokio::test]
    async fn capabilities_reflect_allow_network_and_never_grant_fs_or_secrets() {
        let (ctx, _tmp) = open_ctx("task7-hostcaps", "0.1.0").await;

        let granted = HostInfo::new(&ctx, true);
        assert_eq!(granted.capabilities(), (true, false, false));

        let denied = HostInfo::new(&ctx, false);
        assert_eq!(denied.capabilities(), (false, false, false));
    }
}
