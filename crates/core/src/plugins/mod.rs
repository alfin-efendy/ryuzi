//! Plugin binding layer.
//!
//! A [`ryuzi_plugin_sdk::PluginManifest`] is purely declarative — identity,
//! metadata, settings schema, and so on. This module binds a manifest to the
//! behavioral capabilities it actually provides at runtime
//! ([`host::CorePlugin`]), tracks every installed plugin
//! ([`host::PluginHost`]), and is the new home of [`host::Registries`] (moved
//! here from the deleted `integration` module, which this layer replaces
//! entirely — see `host`'s module doc for how `Registries::add_plugin`
//! supersedes the old `Integration` trait).
//!
//! [`builtin`] holds first-party plugins that don't have a more natural home
//! beside their own implementation module — `native`/`claude-code` live
//! beside their harness code in `harness::native`/`harness::acp`; `discord`
//! lives here since `gateway::discord` is data/protocol-only.
//!
//! [`providers`] and [`runtimes_meta`] generate manifest-only plugins from
//! two existing static catalogs (`llm_router::registry::CATALOG` and
//! `runtimes::CATALOG`) rather than hand-authoring one manifest per entry.
//! [`install_builtins`] adds all of them in one call.

pub mod builtin;
pub mod declarative;
pub mod host;
pub mod providers;
pub mod runtimes_meta;

pub use host::{CorePlugin, PluginHost, PluginSource, Registries};

/// Add every generated manifest-only builtin — every model provider
/// ([`providers::provider_plugins`]) and every CLI agent
/// ([`runtimes_meta::cli_agent_plugins`]) — to `regs`.
///
/// This deliberately does NOT add `native`, `claude-code`, or `discord`:
/// those carry host-injected config (an ACP adapter descriptor, a gateway
/// factory) that only the composition root can supply, so hosts add them
/// first and call `install_builtins` afterward (see `runtimes_meta`'s module
/// doc for why `native` is skipped from the CLI-agent catalog for exactly
/// this reason — it would otherwise collide with the harness plugin).
pub fn install_builtins(regs: &mut Registries) {
    for plugin in providers::provider_plugins() {
        regs.add_plugin(plugin);
    }
    for plugin in runtimes_meta::cli_agent_plugins() {
        regs.add_plugin(plugin);
    }
}

/// Discover and register user-authored plugins from
/// `~/.config/ryuzi/plugins/*/ryuzi-plugin.toml`. Call after
/// [`install_builtins`] so a user manifest can never shadow a built-in
/// (`Registries::add_plugin` keeps the first registration for a given id —
/// see `host`'s module doc).
///
/// A missing config directory is not an error (most installs have none). A
/// plugin directory that fails to parse or fails manifest validation is
/// logged via `tracing::warn!` and skipped — never panics, and never stops
/// the rest of the scan.
pub fn load_user_plugins(regs: &mut Registries) {
    let Some(home) = dirs::home_dir() else {
        tracing::warn!("could not resolve home directory — skipping user plugin discovery");
        return;
    };
    load_user_plugins_from(regs, &home.join(".config/ryuzi/plugins"));
}

/// The scan behind [`load_user_plugins`], factored out so tests can pass a
/// tempdir instead of the real config directory.
pub(crate) fn load_user_plugins_from(regs: &mut Registries, base: &std::path::Path) {
    let Ok(entries) = std::fs::read_dir(base) else {
        return; // no user plugin directory — nothing to do
    };
    for entry in entries.filter_map(Result::ok) {
        let dir = entry.path();
        if !dir.is_dir() {
            continue;
        }
        let manifest_path = dir.join("ryuzi-plugin.toml");
        let text = match std::fs::read_to_string(&manifest_path) {
            Ok(text) => text,
            Err(_) => continue, // no manifest in this directory — not a plugin
        };
        let manifest = match ryuzi_plugin_sdk::PluginManifest::from_toml(&text) {
            Ok(manifest) => manifest,
            Err(e) => {
                tracing::warn!(
                    "skipping user plugin at {}: invalid manifest: {e}",
                    manifest_path.display()
                );
                continue;
            }
        };
        match declarative::declarative_plugin(manifest, PluginSource::User(dir.clone())) {
            Ok(plugin) => regs.add_plugin(plugin),
            Err(e) => {
                tracing::warn!("skipping user plugin at {}: {e}", manifest_path.display());
            }
        }
    }
}

#[cfg(test)]
mod load_user_plugins_tests {
    use super::*;

    const VALID_MANIFEST: &str = r#"
contract = 1
id = "acme-user"
name = "Acme User Plugin"

[[mcp]]
name = "svc"
transport = "stdio"
command = "acme-mcp"
"#;

    fn write_manifest(base: &std::path::Path, plugin_dir: &str, toml_str: &str) {
        let dir = base.join(plugin_dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("ryuzi-plugin.toml"), toml_str).unwrap();
    }

    #[test]
    fn valid_user_manifest_registers_a_connector_capable_plugin_with_user_source() {
        let base = tempfile::tempdir().unwrap();
        write_manifest(base.path(), "acme", VALID_MANIFEST);

        let mut regs = Registries::new();
        load_user_plugins_from(&mut regs, base.path());

        let plugin = regs
            .plugins
            .get("acme-user")
            .expect("valid user manifest should register a plugin");
        assert!(
            plugin.connector.is_some(),
            "manifest has an [[mcp]] entry, so it should be connector-capable"
        );
        assert_eq!(
            plugin.source,
            PluginSource::User(base.path().join("acme")),
            "source should record the manifest's own directory"
        );
    }

    #[test]
    fn broken_toml_is_skipped_without_panicking_and_other_plugins_still_load() {
        let base = tempfile::tempdir().unwrap();
        write_manifest(base.path(), "broken", "this is not valid toml {{{");
        write_manifest(base.path(), "acme", VALID_MANIFEST);

        let mut regs = Registries::new();
        load_user_plugins_from(&mut regs, base.path());

        assert!(
            regs.plugins.get("acme-user").is_some(),
            "the well-formed sibling manifest should still load"
        );
        assert_eq!(
            regs.plugins.list().len(),
            1,
            "the broken manifest must not register anything"
        );
    }

    #[test]
    fn manifest_id_colliding_with_a_builtin_is_skipped_by_add_plugin() {
        let base = tempfile::tempdir().unwrap();
        write_manifest(
            base.path(),
            "fake-anthropic",
            r#"
contract = 1
id = "anthropic"
name = "Fake Anthropic"
"#,
        );

        let mut regs = Registries::new();
        install_builtins(&mut regs); // registers the real "anthropic" provider plugin
        load_user_plugins_from(&mut regs, base.path());

        let plugin = regs.plugins.get("anthropic").unwrap();
        assert_eq!(
            plugin.source,
            PluginSource::Builtin,
            "first registration (the builtin) must win over the colliding user plugin"
        );
    }
}

#[cfg(test)]
mod install_builtins_tests {
    use super::*;
    use crate::harness::acp::AcpAdapterDescriptor;

    fn stub_descriptor() -> AcpAdapterDescriptor {
        AcpAdapterDescriptor {
            command: "true".to_string(),
            args: vec![],
            env: vec![],
            env_remove: vec![],
        }
    }

    #[test]
    fn install_builtins_adds_every_provider_and_cli_agent_id() {
        let mut regs = Registries::new();
        install_builtins(&mut regs);
        let ids: Vec<String> = regs
            .plugins
            .list()
            .iter()
            .map(|p| p.manifest.id.clone())
            .collect();

        for d in crate::llm_router::registry::CATALOG {
            assert!(
                ids.contains(&d.id.to_string()),
                "missing provider plugin for {}",
                d.id
            );
        }
        for d in crate::runtimes::CATALOG {
            if d.id == crate::harness::native::NATIVE_ID
                || crate::llm_router::registry::descriptor(d.id).is_some()
            {
                continue; // claimed elsewhere — see `runtimes_meta`'s module doc
            }
            assert!(
                ids.contains(&d.id.to_string()),
                "missing cli-agent plugin for {}",
                d.id
            );
        }
    }

    #[test]
    fn install_builtins_ids_never_collide_with_native_claude_code_or_discord() {
        let mut regs = Registries::new();
        regs.add_plugin(crate::harness::native::native_plugin());
        regs.add_plugin(crate::harness::acp::claude_code_plugin(stub_descriptor()));
        regs.add_plugin(builtin::discord_plugin());
        assert_eq!(
            regs.plugins.list().len(),
            3,
            "sanity: three builtins registered before install_builtins"
        );

        install_builtins(&mut regs);

        let ids: Vec<String> = regs
            .plugins
            .list()
            .iter()
            .map(|p| p.manifest.id.clone())
            .collect();
        let unique: std::collections::HashSet<&String> = ids.iter().collect();
        assert_eq!(
            unique.len(),
            ids.len(),
            "duplicate plugin ids after install_builtins: {ids:?}"
        );

        // 3 pre-registered (native, claude-code, discord) + every provider +
        // every runtimes-catalog entry EXCEPT `native` (already registered
        // above, under the same id, by the harness plugin) and `ollama`
        // (already covered by the `ollama` model-provider plugin — see
        // `runtimes_meta`'s module doc).
        let expected =
            3 + crate::llm_router::registry::CATALOG.len() + (crate::runtimes::CATALOG.len() - 2);
        assert_eq!(
            ids.len(),
            expected,
            "install_builtins silently dropped a colliding id instead of staying disjoint \
             from the native/claude-code/discord builtins"
        );
    }
}
