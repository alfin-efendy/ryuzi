//! The first-party WASM component bundles shipped from `plugins/<id>`,
//! surfaced as manifest-only [`CorePlugin`]s so they are enumerable through
//! the `list_plugins` RPC that backs Cockpit's Plugins hub.
//!
//! This replaces the removed declarative `plugins::catalog`. Two deliberate
//! differences from that module:
//!
//! - **Manifest-only, always.** A bundle's executable capability (gateway,
//!   connector, provider) is still discovered off disk in
//!   `daemon::build_daemon` from the *installed* bundle root; nothing here
//!   instantiates a component. These entries exist so a bundle is visible
//!   and enumerable even before it is installed.
//! - **Ids a provider builtin already owns are skipped.** Several bundles
//!   under `plugins/` are model providers whose bundle id also appears in
//!   `llm_router::registry::CATALOG`, which [`super::install_providers`]
//!   registers BEFORE this module. [`super::host::PluginHost::add`] is
//!   first-registration-wins, so registering such an id here would be dropped
//!   as a duplicate (and log a warning every boot) while also discarding the
//!   builtin's richer manifest — so [`component_catalog_plugins`] filters them
//!   out via [`provider_registry_owns`]. This covers both the twelve
//!   same-named provider bundles in [`COMPONENT_BACKED_PROVIDER_IDS`] AND
//!   `mimo`/`opencode`, whose bundle ids happen to sit in the CATALOG too.
//!   Every such id is still reported as component-backed by
//!   [`is_component_bundle`], so Cockpit can offer release management for it
//!   against whichever row won.

use ryuzi_plugin_sdk::bundle::PluginBundleManifest;
use ryuzi_plugin_sdk::{PluginManifest, CONTRACT_VERSION};

use super::host::{CorePlugin, PluginSource};

/// The non-colliding first-party bundles, embedded from their in-repo
/// manifest. Keep in sync with the component list in
/// `scripts/plugins/build-first-party.ts`: every id there is either here or
/// in [`COMPONENT_BACKED_PROVIDER_IDS`].
pub const COMPONENT_BUNDLE_MANIFESTS: &[(&str, &str)] = &[
    (
        "github",
        include_str!("../../../../plugins/github/ryuzi-plugin.toml"),
    ),
    (
        "atlassian",
        include_str!("../../../../plugins/atlassian/ryuzi-plugin.toml"),
    ),
    (
        "bitbucket",
        include_str!("../../../../plugins/bitbucket/ryuzi-plugin.toml"),
    ),
    (
        "discord",
        include_str!("../../../../plugins/discord/ryuzi-plugin.toml"),
    ),
    (
        "mimo",
        include_str!("../../../../plugins/mimo/ryuzi-plugin.toml"),
    ),
    (
        "opencode",
        include_str!("../../../../plugins/opencode/ryuzi-plugin.toml"),
    ),
];

/// Whether `id` is already owned by a `llm_router::registry::CATALOG`
/// provider, which [`super::install_providers`] registers BEFORE this module.
/// Such an id is skipped here rather than handed to `PluginHost::add`, which
/// would drop it as a duplicate and log a warning on every boot.
fn provider_registry_owns(id: &str) -> bool {
    crate::llm_router::registry::CATALOG
        .iter()
        .any(|d| d.id == id)
}

/// Every first-party component bundle id — the embedded manifests plus the
/// provider bundles represented by their builtin row. Used to flag a plugin
/// as component-backed in `PluginInfo` so Cockpit's release-management surface
/// (install / active version / rollback) can find it regardless of which
/// registration won the id.
pub fn is_component_bundle(id: &str) -> bool {
    COMPONENT_BUNDLE_MANIFESTS.iter().any(|(got, _)| *got == id)
        || COMPONENT_BACKED_PROVIDER_IDS.contains(&id)
}

/// Provider bundles that exist under `plugins/` but are represented in the
/// plugin list by their `install_providers` builtin instead, because the
/// bundle id and the router provider id are the same string (see module doc).
pub const COMPONENT_BACKED_PROVIDER_IDS: &[&str] = &[
    "anthropic",
    "anthropic-oauth",
    "openai",
    "openrouter",
    "groq",
    "deepseek",
    "mistral",
    "xai",
    "nvidia",
    "huggingface",
    "google",
    "qwen",
];

/// Map a bundle manifest onto the declarative [`PluginManifest`] shape the
/// plugin list speaks. Fields the bundle format has no concept of (`auth`,
/// `mcp`, `settings`, `skills`, `provider`) stay empty on purpose: a
/// component's behavior is contributed by the running component, never by
/// this manifest.
fn manifest_from_bundle(bundle: PluginBundleManifest) -> PluginManifest {
    PluginManifest {
        contract: CONTRACT_VERSION,
        id: bundle.id,
        name: bundle.name,
        version: bundle.version,
        publisher: if bundle.publisher.is_empty() {
            "Ryuzi".to_string()
        } else {
            bundle.publisher
        },
        description: bundle.description,
        homepage: None,
        icon: None,
        categories: vec!["component".to_string()],
        slot: None,
        verified: true,
        experimental: false,
        auth: None,
        settings: vec![],
        mcp: vec![],
        extensions: vec![],
        skills: vec![],
        provider: None,
    }
}

/// Every embedded component bundle as a manifest-only plugin. A manifest that
/// fails to parse is logged and skipped rather than panicking, so one bad
/// embedded file can never take the daemon down at startup.
pub fn component_catalog_plugins() -> Vec<CorePlugin> {
    COMPONENT_BUNDLE_MANIFESTS
        .iter()
        .filter(|(id, _)| !provider_registry_owns(id))
        .filter_map(
            |(id, src)| match toml::from_str::<PluginBundleManifest>(src) {
                Ok(bundle) => Some(CorePlugin {
                    manifest: manifest_from_bundle(bundle),
                    harness: None,
                    gateway: None,
                    connector: None,
                    extension: None,
                    provider: None,
                    source: PluginSource::Component,
                }),
                Err(error) => {
                    tracing::error!("component catalog: manifest `{id}` failed to parse: {error}");
                    None
                }
            },
        )
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_embedded_manifest_parses_and_matches_its_declared_id() {
        for (id, toml_src) in COMPONENT_BUNDLE_MANIFESTS {
            let manifest: PluginBundleManifest = toml::from_str(toml_src)
                .unwrap_or_else(|e| panic!("component manifest `{id}` failed to parse: {e}"));
            assert_eq!(&manifest.id, id, "declared id must match the embedded slot");
        }
    }

    // `mimo`/`opencode` are embedded (their manifests are real) but ALSO live
    // in the router CATALOG, so they are represented by their provider builtin
    // and skipped here rather than dropped as duplicates at registration.
    #[test]
    fn registers_only_components_no_provider_builtin_already_owns() {
        let plugins = component_catalog_plugins();
        let mut ids: Vec<&str> = plugins.iter().map(|p| p.manifest.id.as_str()).collect();
        ids.sort_unstable();
        assert_eq!(ids, ["atlassian", "bitbucket", "discord", "github"]);
    }

    // Every bundle stays reachable for release management even when its
    // registration lost the id to a provider builtin.
    #[test]
    fn is_component_bundle_covers_embedded_and_provider_backed_ids() {
        for id in [
            "github",
            "atlassian",
            "bitbucket",
            "discord",
            "mimo",
            "opencode",
        ] {
            assert!(is_component_bundle(id), "`{id}` is a first-party bundle");
        }
        for id in COMPONENT_BACKED_PROVIDER_IDS {
            assert!(is_component_bundle(id), "`{id}` is a first-party bundle");
        }
        assert!(!is_component_bundle("native"));
        assert!(!is_component_bundle("nope"));
    }

    // A colliding provider component shares an id with an `install_providers`
    // plugin, which registers FIRST and wins. Registering one here would be
    // silently dropped by `PluginHost::add`, so they are excluded by design.
    #[test]
    fn colliding_provider_components_are_not_registered() {
        let plugins = component_catalog_plugins();
        for id in COMPONENT_BACKED_PROVIDER_IDS {
            assert!(
                !plugins.iter().any(|p| p.manifest.id == *id),
                "provider component `{id}` must not be registered — it collides with its builtin"
            );
        }
    }

    #[test]
    fn every_plugin_is_manifest_only_and_component_sourced() {
        for plugin in component_catalog_plugins() {
            assert_eq!(plugin.source, PluginSource::Component);
            assert!(plugin.connector.is_none(), "manifest-only registration");
            assert!(plugin.gateway.is_none(), "manifest-only registration");
            assert!(plugin.harness.is_none(), "manifest-only registration");
            assert!(plugin.provider.is_none(), "manifest-only registration");
        }
    }

    // The embedded set and the excluded-provider set must together cover every
    // component `scripts/plugins/build-first-party.ts` builds and signs, or a
    // newly added bundle would silently never appear in the Plugins hub.
    #[test]
    fn embedded_and_excluded_sets_are_disjoint() {
        for (id, _) in COMPONENT_BUNDLE_MANIFESTS {
            assert!(
                !COMPONENT_BACKED_PROVIDER_IDS.contains(id),
                "`{id}` cannot be both embedded and excluded"
            );
        }
    }
}
