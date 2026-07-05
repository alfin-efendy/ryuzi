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
