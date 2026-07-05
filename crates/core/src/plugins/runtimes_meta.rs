//! CLI-agent plugins: every entry in `runtimes::CATALOG` surfaced as a
//! manifest-only [`CorePlugin`] — no harness/gateway/connector capability of
//! its own, purely so an agent CLI shows up in the plugin host/catalog UI.
//!
//! Two kinds of `runtimes::CATALOG` entries are deliberately skipped, both
//! because they'd collide with a plugin id that already means something
//! else in the shared `PluginHost` namespace:
//!
//! - `native`: already has a full harness-backed plugin
//!   (`harness::native::native_plugin`, id
//!   [`NATIVE_ID`](crate::harness::native::NATIVE_ID)) with real behavioral
//!   capability.
//! - `ollama`: the local Ollama integration is already surfaced as a
//!   `llm_router::registry::CATALOG` model-provider plugin (same id, real
//!   `base_url`/`models`) by [`super::providers`] — the runtimes-catalog
//!   entry is Cockpit's "detect the binary, list installed models" view of
//!   the same underlying service, not a distinct identity.
//!
//! In both cases mapping the runtimes-catalog entry too would register a
//! second, weaker manifest under an id some other plugin already owns —
//! `PluginHost::add` keeps the first registration and silently drops the
//! duplicate, which would make that id's plugin entry depend on
//! registration order instead of always being the richer one. This module
//! filters generically (any id also present in the provider catalog is
//! skipped) so a future catalog addition that happens to share an id fails
//! the same way rather than silently producing a dropped duplicate.

use ryuzi_plugin_sdk::{PluginManifest, RuntimeMeta, CONTRACT_VERSION};

use crate::harness::native::NATIVE_ID;
use crate::llm_router::registry;
use crate::runtimes::{RuntimeDescriptor, CATALOG};

use super::host::{CorePlugin, PluginSource};

fn cli_agent_plugin(d: &RuntimeDescriptor) -> CorePlugin {
    CorePlugin {
        manifest: PluginManifest {
            contract: CONTRACT_VERSION,
            id: d.id.to_string(),
            name: d.name.to_string(),
            version: "0.0.0".to_string(),
            publisher: "ryuzi".to_string(),
            description: format!("{} — CLI coding agent Cockpit can drive.", d.name),
            homepage: None,
            icon: None,
            categories: vec!["cli-agent".to_string()],
            verified: true,
            experimental: false,
            auth: None,
            settings: vec![],
            mcp: vec![],
            skills: vec![],
            menu: None,
            provider: None,
            runtime: Some(RuntimeMeta {
                binary: Some(d.binary.to_string()),
                npm_package: d.npm_package.map(|s| s.to_string()),
                default_model: (!d.default_model.is_empty()).then(|| d.default_model.to_string()),
            }),
        },
        harness: None,
        gateway: None,
        connector: None,
        source: PluginSource::Builtin,
    }
}

/// Whether `id` already has a plugin identity elsewhere in the shared
/// `PluginHost` namespace — either the `native` harness builtin, or a
/// `llm_router::registry::CATALOG` model-provider plugin (see module doc).
fn already_claimed_elsewhere(id: &str) -> bool {
    id == NATIVE_ID || registry::descriptor(id).is_some()
}

/// Every `runtimes::CATALOG` CLI agent as a manifest-only plugin, except ids
/// already claimed elsewhere (see module doc and
/// [`already_claimed_elsewhere`]).
pub fn cli_agent_plugins() -> Vec<CorePlugin> {
    CATALOG
        .iter()
        .filter(|d| !already_claimed_elsewhere(d.id))
        .map(cli_agent_plugin)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_unclaimed_catalog_runtime_is_mapped() {
        let plugins = cli_agent_plugins();
        let expected: Vec<&str> = CATALOG
            .iter()
            .map(|d| d.id)
            .filter(|id| !already_claimed_elsewhere(id))
            .collect();
        assert_eq!(plugins.len(), expected.len());
        for id in expected {
            assert!(
                plugins.iter().any(|p| p.manifest.id == id),
                "missing cli-agent plugin for {id}"
            );
        }
    }

    #[test]
    fn native_is_excluded_because_the_harness_plugin_already_owns_that_id() {
        assert!(cli_agent_plugins()
            .iter()
            .all(|p| p.manifest.id != NATIVE_ID));
    }

    #[test]
    fn ollama_is_excluded_because_it_is_already_a_model_provider_plugin() {
        assert!(cli_agent_plugins()
            .iter()
            .all(|p| p.manifest.id != "ollama"));
        // Sanity: this is really a collision in the underlying catalogs, not
        // a coincidence of the filter — confirm the id exists in both.
        assert!(CATALOG.iter().any(|d| d.id == "ollama"));
        assert!(registry::descriptor("ollama").is_some());
    }

    #[test]
    fn claude_manifest_has_expected_shape() {
        let plugin = cli_agent_plugins()
            .into_iter()
            .find(|p| p.manifest.id == "claude")
            .expect("claude plugin");

        assert_eq!(plugin.manifest.contract, CONTRACT_VERSION);
        assert_eq!(plugin.manifest.name, "Claude Code");
        assert_eq!(plugin.manifest.publisher, "ryuzi");
        assert!(plugin.manifest.verified);
        assert_eq!(plugin.manifest.categories, vec!["cli-agent".to_string()]);

        let runtime = plugin.manifest.runtime.expect("runtime block");
        assert_eq!(runtime.binary.as_deref(), Some("claude"));
        assert_eq!(
            runtime.npm_package.as_deref(),
            Some("@anthropic-ai/claude-code")
        );
        assert_eq!(runtime.default_model.as_deref(), Some("claude-opus-4-5"));

        assert!(plugin.harness.is_none());
        assert!(plugin.gateway.is_none());
        assert!(plugin.connector.is_none());
    }

    #[test]
    fn empty_default_model_becomes_none() {
        // `opencode` survives the collision filter (unlike `ollama`) and has
        // an empty `default_model` in the catalog.
        let plugin = cli_agent_plugins()
            .into_iter()
            .find(|p| p.manifest.id == "opencode")
            .expect("opencode plugin");
        let runtime = plugin.manifest.runtime.unwrap();
        assert_eq!(runtime.default_model, None);
        assert_eq!(runtime.npm_package.as_deref(), Some("opencode-ai"));
    }

    #[test]
    fn missing_npm_package_becomes_none() {
        // Every surviving catalog entry happens to have an npm package, so
        // exercise the mapping function directly against a synthetic
        // descriptor rather than relying on catalog data for this case.
        let synthetic = RuntimeDescriptor {
            id: "synthetic",
            name: "Synthetic",
            color: "#000000",
            initial: "S",
            connection: "test",
            binary: "synthetic-bin",
            npm_package: None,
            models: &[],
            default_model: "",
            tiers: &[],
        };
        let plugin = cli_agent_plugin(&synthetic);
        let runtime = plugin.manifest.runtime.unwrap();
        assert_eq!(runtime.npm_package, None);
        assert_eq!(runtime.binary.as_deref(), Some("synthetic-bin"));
    }
}
