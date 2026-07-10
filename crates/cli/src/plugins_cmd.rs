//! `ryuzi plugins` — list, inspect, enable, and disable installed plugins.
//! Thin: parse → act → print via `deps.out`/`deps.err`, mirroring
//! `config_cmd.rs`. Every plugin comes from `deps.build_registries`'s
//! `Registries.plugins` (a [`ryuzi_core::PluginHost`]); enable/disable
//! delegates to [`ryuzi_core::plugins::toggle_enabled`], the single source of
//! truth shared with the Cockpit `set_plugin_enabled` command, which mirrors
//! the enablement rules `PluginHost::is_enabled` reads back (see that
//! method's doc): harness-capable plugins toggle the `enabled_runtimes` CSV,
//! gateway-capable toggle `enabled_gateways`, and everything else toggles its
//! own `plugin.<id>.enabled` flag.

use std::sync::Arc;

use ryuzi_core::settings::SettingsStore;

use crate::dispatch::Deps;

pub fn cmd_plugins(args: &[String], deps: &mut Deps) -> u8 {
    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .expect("tokio runtime");
    rt.block_on(plugins_inner(args, deps))
}

async fn plugins_inner(args: &[String], deps: &mut Deps) -> u8 {
    match args.first().map(String::as_str) {
        Some("list") => cmd_list(deps).await,
        Some("info") => match args.get(1) {
            Some(id) => cmd_info(id, deps).await,
            None => usage(deps, "usage: ryuzi plugins info <id>"),
        },
        Some("enable") => match args.get(1) {
            Some(id) => cmd_set_enabled(id, true, deps).await,
            None => usage(deps, "usage: ryuzi plugins enable <id>"),
        },
        Some("disable") => match args.get(1) {
            Some(id) => cmd_set_enabled(id, false, deps).await,
            None => usage(deps, "usage: ryuzi plugins disable <id>"),
        },
        _ => usage(deps, "usage: ryuzi plugins <list|info|enable|disable> ..."),
    }
}

fn usage(deps: &mut Deps, message: &str) -> u8 {
    (deps.err)(message);
    2
}

/// `verified` wins over `experimental`; anything else is community-authored.
fn status_label(verified: bool, experimental: bool) -> &'static str {
    if verified {
        "verified"
    } else if experimental {
        "experimental"
    } else {
        "community"
    }
}

async fn cmd_list(deps: &mut Deps) -> u8 {
    let registries = match (deps.build_registries)() {
        Ok(r) => r,
        Err(e) => {
            (deps.err)(&format!("✗ {e}"));
            return 1;
        }
    };
    let settings = match open_settings(deps).await {
        Ok(s) => s,
        Err(e) => {
            (deps.err)(&format!("✗ {e}"));
            return 1;
        }
    };

    for plugin in registries.plugins.list() {
        let id = plugin.manifest.id.clone();
        let enabled = match registries.plugins.is_enabled(&settings, &id).await {
            Ok(b) => b,
            Err(e) => {
                (deps.err)(&format!("✗ {e}"));
                return 1;
            }
        };
        (deps.out)(&format!(
            "{}\t{}\t{}\t{}\t{}",
            id,
            plugin.manifest.name,
            plugin.manifest.categories.join(","),
            if enabled { "enabled" } else { "disabled" },
            status_label(plugin.manifest.verified, plugin.manifest.experimental),
        ));
    }
    0
}

async fn cmd_info(id: &str, deps: &mut Deps) -> u8 {
    let registries = match (deps.build_registries)() {
        Ok(r) => r,
        Err(e) => {
            (deps.err)(&format!("✗ {e}"));
            return 1;
        }
    };
    let Some(plugin) = registries.plugins.get(id) else {
        (deps.err)(&format!("unknown plugin: {id}"));
        return 1;
    };
    let settings = match open_settings(deps).await {
        Ok(s) => s,
        Err(e) => {
            (deps.err)(&format!("✗ {e}"));
            return 1;
        }
    };
    let enabled = match registries.plugins.is_enabled(&settings, id).await {
        Ok(b) => b,
        Err(e) => {
            (deps.err)(&format!("✗ {e}"));
            return 1;
        }
    };

    let m = &plugin.manifest;
    (deps.out)(&format!("id: {}", m.id));
    (deps.out)(&format!("name: {}", m.name));
    (deps.out)(&format!("version: {}", m.version));
    (deps.out)(&format!("publisher: {}", m.publisher));
    (deps.out)(&format!("description: {}", m.description));
    (deps.out)(&format!("categories: {}", m.categories.join(",")));
    (deps.out)(&format!(
        "status: {}",
        status_label(m.verified, m.experimental)
    ));

    let capabilities = plugin.capabilities();
    (deps.out)(&format!(
        "capabilities: {}",
        if capabilities.is_empty() {
            "manifest-only".to_string()
        } else {
            capabilities.join(",")
        }
    ));
    (deps.out)(&format!(
        "enabled: {}",
        if enabled { "enabled" } else { "disabled" }
    ));

    // Auth: identity only — the kind, which setting/env carry the secret,
    // and where to get one. NEVER the secret value itself.
    if let Some(auth) = &m.auth {
        (deps.out)(&format!(
            "auth: kind={:?} setting={} env={} help_url={}",
            auth.kind,
            auth.setting.as_deref().unwrap_or("-"),
            auth.env.as_deref().unwrap_or("-"),
            auth.help_url.as_deref().unwrap_or("-"),
        ));
    }

    for field in &m.settings {
        (deps.out)(&format!(
            "setting: {} label=\"{}\" secret={}",
            field.key, field.label, field.secret
        ));
    }

    // MCP servers: raw manifest strings verbatim — no `${auth}` substitution.
    for server in &m.mcp {
        let target = server
            .command
            .as_deref()
            .or(server.url.as_deref())
            .unwrap_or("-");
        (deps.out)(&format!(
            "mcp: {} transport={:?} target={}",
            server.name, server.transport, target
        ));
    }

    if let Some(provider) = &m.provider {
        let models = provider
            .models
            .iter()
            .map(|model| {
                if model.default {
                    format!("{}(default)", model.id)
                } else {
                    model.id.clone()
                }
            })
            .collect::<Vec<_>>()
            .join(",");
        (deps.out)(&format!(
            "provider: format={} base_url={} models={}",
            provider.format,
            provider.base_url.as_deref().unwrap_or("-"),
            models
        ));
    }

    if let Some(runtime) = &m.runtime {
        (deps.out)(&format!(
            "runtime: binary={} npm_package={} default_model={}",
            runtime.binary.as_deref().unwrap_or("-"),
            runtime.npm_package.as_deref().unwrap_or("-"),
            runtime.default_model.as_deref().unwrap_or("-"),
        ));
    }

    0
}

async fn cmd_set_enabled(id: &str, enable: bool, deps: &mut Deps) -> u8 {
    let registries = match (deps.build_registries)() {
        Ok(r) => r,
        Err(e) => {
            (deps.err)(&format!("✗ {e}"));
            return 1;
        }
    };
    let settings = match open_settings(deps).await {
        Ok(s) => s,
        Err(e) => {
            (deps.err)(&format!("✗ {e}"));
            return 1;
        }
    };

    // Single source of truth for the CSV/flag toggle rules shared with the
    // Cockpit `set_plugin_enabled` command — see `ryuzi_core::plugins::
    // toggle_enabled`'s doc for the harness/gateway/flag priority order.
    match ryuzi_core::plugins::toggle_enabled(&registries.plugins, &settings, id, enable).await {
        Ok(()) => {
            (deps.out)(&format!(
                "{} {id}",
                if enable { "enabled" } else { "disabled" }
            ));
            0
        }
        Err(e) => {
            (deps.err)(&e.to_string());
            1
        }
    }
}

async fn open_settings(deps: &mut Deps) -> anyhow::Result<SettingsStore> {
    Ok(SettingsStore::new(Arc::new(
        crate::db::open_store(deps).await?,
    )))
}
