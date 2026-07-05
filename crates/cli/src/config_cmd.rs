use std::sync::Arc;

use ryuzi_core::settings::{all_fields, is_secret, SettingsStore};

use crate::dispatch::Deps;

fn redact(key: &str, value: &str) -> String {
    if is_secret(key) {
        "•".repeat(8)
    } else {
        value.to_string()
    }
}

pub fn cmd_config(args: &[String], deps: &mut Deps) -> u8 {
    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .expect("tokio runtime");
    rt.block_on(config_inner(args, deps))
}

async fn config_inner(args: &[String], deps: &mut Deps) -> u8 {
    // `ryuzi config` doesn't go through `deps.build_registries` (that would
    // pull in the claude-code ACP sidecar resolution and its noisy
    // `eprintln!` note on failure — see that closure in `main.rs`). Without
    // this, the process-wide `plugin.*` settings registry stays empty here:
    // `config set plugin.<id>.<key> ...` would fail "unknown setting", and
    // `config get` would report `is_secret` as `false` for a real plugin
    // secret and print it unredacted.
    ryuzi_core::plugins::register_builtin_plugin_fields();
    match args.first().map(String::as_str) {
        Some("get") => {
            let rest = &args[1..];
            let reveal = rest.iter().any(|a| a == "--reveal");
            let Some(key) = rest.iter().find(|a| *a != "--reveal") else {
                (deps.err)("usage: ryuzi config get <key> [--reveal]");
                return 1;
            };
            let settings = match open_settings(deps).await {
                Ok(s) => s,
                Err(e) => {
                    (deps.err)(&format!("✗ {e}"));
                    return 1;
                }
            };
            let value = match settings.get(key).await {
                Ok(v) => v.unwrap_or_default(),
                Err(e) => {
                    (deps.err)(&format!("✗ {e}"));
                    return 1;
                }
            };
            (deps.out)(&if reveal || value.is_empty() {
                value.clone()
            } else {
                redact(key, &value)
            });
            0
        }
        Some("set") => {
            let (Some(key), Some(value)) = (args.get(1), args.get(2)) else {
                (deps.err)("usage: ryuzi config set <key> <value>");
                return 1;
            };
            let settings = match open_settings(deps).await {
                Ok(s) => s,
                Err(e) => {
                    (deps.err)(&format!("✗ {e}"));
                    return 1;
                }
            };
            match settings.set(key, value).await {
                Ok(()) => {
                    (deps.out)(&format!("set {key}"));
                    0
                }
                Err(e) => {
                    (deps.err)(&e.to_string());
                    1
                }
            }
        }
        Some("list") => {
            let settings = match open_settings(deps).await {
                Ok(s) => s,
                Err(e) => {
                    (deps.err)(&format!("✗ {e}"));
                    return 1;
                }
            };
            let persisted = match settings.list().await {
                Ok(p) => p,
                Err(e) => {
                    (deps.err)(&format!("✗ {e}"));
                    return 1;
                }
            };
            for field in all_fields() {
                match (persisted.get(field.key), field.default) {
                    (Some(v), _) => {
                        (deps.out)(&format!("{} = {}", field.key, redact(field.key, v)))
                    }
                    (None, Some(d)) => (deps.out)(&format!(
                        "{} = {} (default)",
                        field.key,
                        redact(field.key, d)
                    )),
                    (None, None) => (deps.out)(&format!("{} = (unset)", field.key)),
                }
            }
            0
        }
        _ => {
            (deps.err)("usage: ryuzi config <get|set|list> ...");
            1
        }
    }
}

async fn open_settings(deps: &mut Deps) -> anyhow::Result<SettingsStore> {
    Ok(SettingsStore::new(Arc::new(
        crate::db::open_store(deps).await?,
    )))
}
