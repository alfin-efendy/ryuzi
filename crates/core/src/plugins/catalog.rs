//! The embedded integration catalog: ~24 third-party connectors (GitHub,
//! Atlassian, Notion, memory backends, sandboxes, tunnels, deploy platforms,
//! and more) shipped as TOML manifests baked into the binary via
//! `include_str!`, rather than hand-written Rust per integration — see
//! `ryuzi_plugin_sdk::PluginManifest` and `super::declarative` for why a
//! manifest with `[[mcp]]` entries needs no bespoke connector code.
//!
//! Every manifest lives in `crates/core/plugins/catalog/<id>.toml` (a
//! sibling of `src/`, not under it — these are data files, not Rust source)
//! and is registered in [`CATALOG_MANIFESTS`] below. [`catalog_plugins`]
//! parses and validates every one of them eagerly; a broken embedded
//! manifest is a build-time bug (it shipped inside the binary, so there is
//! no "skip and log" recovery the way `plugins::load_skill_pack_plugins_from`
//! recovers from a bad on-disk user manifest) — hence the `expect()` naming
//! the offending id.
//!
//! All catalog plugins are connector-only (no harness/gateway capability),
//! so `PluginHost::is_enabled` treats them like any other manifest-only or
//! connector-only plugin: disabled until `plugin.<id>.enabled=true` is set
//! (see `plugins::toggle_enabled`'s doc).

use ryuzi_plugin_sdk::PluginManifest;

use super::declarative::declarative_plugin;
use super::host::{CorePlugin, PluginSource};

/// Every embedded catalog manifest's id paired with its raw TOML text.
/// Ordering matches the catalog's grouping in the MCP fleet research doc
/// (VCS/issues, docs/productivity, communication, design/search,
/// observability, memory, sandboxes, tunnels, deploy) purely for readability
/// — nothing depends on this order.
pub const CATALOG_MANIFESTS: &[(&str, &str)] = &[
    ("github", include_str!("../../plugins/catalog/github.toml")),
    (
        "atlassian",
        include_str!("../../plugins/catalog/atlassian.toml"),
    ),
    ("notion", include_str!("../../plugins/catalog/notion.toml")),
    ("linear", include_str!("../../plugins/catalog/linear.toml")),
    (
        "google-workspace",
        include_str!("../../plugins/catalog/google-workspace.toml"),
    ),
    (
        "telegram",
        include_str!("../../plugins/catalog/telegram.toml"),
    ),
    ("slack", include_str!("../../plugins/catalog/slack.toml")),
    ("figma", include_str!("../../plugins/catalog/figma.toml")),
    (
        "brave-search",
        include_str!("../../plugins/catalog/brave-search.toml"),
    ),
    ("sentry", include_str!("../../plugins/catalog/sentry.toml")),
    (
        "datadog",
        include_str!("../../plugins/catalog/datadog.toml"),
    ),
    ("mem0", include_str!("../../plugins/catalog/mem0.toml")),
    ("zep", include_str!("../../plugins/catalog/zep.toml")),
    ("honcho", include_str!("../../plugins/catalog/honcho.toml")),
    (
        "graphiti",
        include_str!("../../plugins/catalog/graphiti.toml"),
    ),
    (
        "cavemem",
        include_str!("../../plugins/catalog/cavemem.toml"),
    ),
    (
        "daytona",
        include_str!("../../plugins/catalog/daytona.toml"),
    ),
    ("e2b", include_str!("../../plugins/catalog/e2b.toml")),
    (
        "cloudflare",
        include_str!("../../plugins/catalog/cloudflare.toml"),
    ),
    ("ngrok", include_str!("../../plugins/catalog/ngrok.toml")),
    ("vercel", include_str!("../../plugins/catalog/vercel.toml")),
    (
        "vercel-sandbox",
        include_str!("../../plugins/catalog/vercel-sandbox.toml"),
    ),
    ("render", include_str!("../../plugins/catalog/render.toml")),
    (
        "netlify",
        include_str!("../../plugins/catalog/netlify.toml"),
    ),
];

/// Parse, validate, and bind every embedded catalog manifest into a
/// connector-capable (or, for the docs-only experimental entries, capability-
/// less) [`CorePlugin`]. Panics (via `expect`, naming the offending id) on a
/// parse or validation failure — an embedded manifest that fails to load is
/// a bug in this build, not a runtime condition callers should handle.
pub fn catalog_plugins() -> Vec<CorePlugin> {
    CATALOG_MANIFESTS
        .iter()
        .map(|(id, toml_str)| {
            let manifest = PluginManifest::from_toml(toml_str).unwrap_or_else(|e| {
                panic!("embedded catalog manifest {id:?} failed to parse: {e}")
            });
            declarative_plugin(manifest, PluginSource::Catalog).unwrap_or_else(|e| {
                panic!("embedded catalog manifest {id:?} failed to validate: {e}")
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use ryuzi_plugin_sdk::categories;
    use std::collections::HashSet;

    /// Every id that already has a plugin identity elsewhere in the shared
    /// `PluginHost` namespace before catalog plugins are added — mirrors
    /// `install_builtins_tests::install_builtins_ids_never_collide_with_native_claude_code_or_discord`'s
    /// fixture set (native/claude-code/discord builtins) plus every
    /// provider/cli-agent id from `install_builtins` itself, since those are
    /// the ids that any catalog manifest could collide with in a real host.
    fn all_code_builtin_ids() -> HashSet<String> {
        let mut ids: HashSet<String> = HashSet::new();
        ids.insert(crate::harness::native::NATIVE_ID.to_string());
        ids.insert("claude-code".to_string());
        ids.insert("discord".to_string());
        for d in crate::llm_router::registry::CATALOG {
            ids.insert(d.id.to_string());
        }
        for d in crate::runtimes::CATALOG {
            ids.insert(d.id.to_string());
        }
        ids
    }

    #[test]
    fn every_embedded_manifest_parses_and_validates() {
        for (id, toml_str) in CATALOG_MANIFESTS {
            let manifest = PluginManifest::from_toml(toml_str)
                .unwrap_or_else(|e| panic!("{id} failed to parse/validate: {e}"));
            assert_eq!(&manifest.id, id, "manifest id must match its catalog key");
        }
    }

    #[test]
    fn catalog_plugins_builds_one_core_plugin_per_manifest() {
        let plugins = catalog_plugins();
        assert_eq!(plugins.len(), CATALOG_MANIFESTS.len());
        for (id, _) in CATALOG_MANIFESTS {
            assert!(
                plugins.iter().any(|p| p.manifest.id == *id),
                "missing catalog plugin for {id}"
            );
        }
    }

    #[test]
    fn catalog_has_exactly_twenty_four_entries() {
        assert_eq!(CATALOG_MANIFESTS.len(), 24);
    }

    #[test]
    fn catalog_ids_are_unique() {
        let ids: Vec<&str> = CATALOG_MANIFESTS.iter().map(|(id, _)| *id).collect();
        let unique: HashSet<&str> = ids.iter().copied().collect();
        assert_eq!(unique.len(), ids.len(), "duplicate catalog ids: {ids:?}");
    }

    #[test]
    fn catalog_ids_never_collide_with_a_code_builtin() {
        let builtins = all_code_builtin_ids();
        for (id, _) in CATALOG_MANIFESTS {
            assert!(
                !builtins.contains(*id),
                "catalog id {id} collides with a code builtin"
            );
        }
    }

    #[test]
    fn every_non_experimental_entry_has_mcp_and_menu() {
        for plugin in catalog_plugins() {
            if plugin.manifest.experimental {
                continue;
            }
            let id = &plugin.manifest.id;
            assert!(
                !plugin.manifest.mcp.is_empty(),
                "{id} is not experimental so it must declare at least one [[mcp]] server"
            );
            assert!(
                plugin.manifest.menu.is_some(),
                "{id} is not experimental so it must declare a [menu] contribution"
            );
        }
    }

    #[test]
    fn every_experimental_entry_has_no_mcp() {
        for plugin in catalog_plugins() {
            if !plugin.manifest.experimental {
                continue;
            }
            assert!(
                plugin.manifest.mcp.is_empty(),
                "{} is experimental so it must not declare an [[mcp]] server",
                plugin.manifest.id
            );
        }
    }

    #[test]
    fn exactly_ngrok_vercel_sandbox_and_zep_are_experimental() {
        let experimental: HashSet<String> = catalog_plugins()
            .into_iter()
            .filter(|p| p.manifest.experimental)
            .map(|p| p.manifest.id)
            .collect();
        assert_eq!(
            experimental,
            HashSet::from([
                "ngrok".to_string(),
                "vercel-sandbox".to_string(),
                "zep".to_string(),
            ])
        );
    }

    #[test]
    fn every_category_is_known() {
        for plugin in catalog_plugins() {
            assert!(
                plugin.manifest.warnings().is_empty(),
                "{} has categories outside categories::KNOWN: {:?}",
                plugin.manifest.id,
                plugin.manifest.categories
            );
            for category in &plugin.manifest.categories {
                assert!(
                    categories::KNOWN.contains(&category.as_str()),
                    "{} declares unknown category {category}",
                    plugin.manifest.id
                );
            }
        }
    }

    #[test]
    fn every_auth_block_with_a_credential_kind_has_a_help_url() {
        for plugin in catalog_plugins() {
            let Some(auth) = &plugin.manifest.auth else {
                continue;
            };
            if auth.kind == ryuzi_plugin_sdk::AuthKind::None {
                continue;
            }
            assert!(
                auth.help_url.is_some(),
                "{} declares [auth] with kind {:?} but no help_url",
                plugin.manifest.id,
                auth.kind
            );
        }
    }

    #[test]
    fn every_catalog_plugin_is_connector_capable_or_docs_only() {
        for plugin in catalog_plugins() {
            assert!(
                plugin.harness.is_none(),
                "{} must not be a harness",
                plugin.manifest.id
            );
            assert!(
                plugin.gateway.is_none(),
                "{} must not be a gateway",
                plugin.manifest.id
            );
            if plugin.manifest.mcp.is_empty() {
                assert!(
                    plugin.connector.is_none(),
                    "{} has no [[mcp]] servers so it must have no connector",
                    plugin.manifest.id
                );
            } else {
                assert!(
                    plugin.connector.is_some(),
                    "{} declares [[mcp]] servers so it must be connector-capable",
                    plugin.manifest.id
                );
            }
        }
    }

    /// The archived/dead packages and endpoints the research doc's "Key
    /// corrections" section calls out — none of them should ever appear in
    /// the embedded catalog text, since shipping one would silently regress
    /// to a dead server.
    const ARCHIVED_REFERENCES: &[&str] = &[
        "@modelcontextprotocol/server-github",
        "@modelcontextprotocol/server-brave-search",
        "@modelcontextprotocol/server-slack",
        "@e2b/mcp-server",
        "mcp.atlassian.com/v1/sse",
        "mcp.linear.app/sse",
    ];

    #[test]
    fn no_manifest_references_an_archived_package_or_endpoint() {
        for (id, toml_str) in CATALOG_MANIFESTS {
            for archived in ARCHIVED_REFERENCES {
                assert!(
                    !toml_str.contains(archived),
                    "{id}'s manifest references archived/dead {archived} — see the MCP fleet \
                     research doc's Key corrections section"
                );
            }
        }
    }

    #[test]
    fn vercel_opts_into_dynamic_registration() {
        let vercel = catalog_plugins()
            .into_iter()
            .find(|p| p.manifest.id == "vercel")
            .expect("vercel catalog plugin");
        assert!(
            vercel
                .manifest
                .auth
                .as_ref()
                .expect("vercel declares [auth]")
                .dynamic_registration,
            "vercel must attempt DCR (failure falls back to the manual client-id form)"
        );
    }
}
