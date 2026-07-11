use std::path::Path;
use std::sync::Arc;

use async_trait::async_trait;
use predicates::prelude::*;
use ryuzi_core::{Connector, ConnectorCtx, CorePlugin, McpServerSpec, PluginSource, Registries};
use ryuzi_plugin_sdk::PluginManifest;

/// A connector that contributes no MCP servers — enough to exercise the
/// connector-only branch of `PluginHost::is_enabled` (`plugin.<id>.enabled`)
/// without depending on any real integration.
struct NoopConnector;

#[async_trait]
impl Connector for NoopConnector {
    async fn mcp_servers(&self, _ctx: &ConnectorCtx) -> anyhow::Result<Vec<McpServerSpec>> {
        Ok(vec![])
    }
}

fn minimal_manifest(id: &str, name: &str) -> PluginManifest {
    PluginManifest {
        contract: 1,
        id: id.to_string(),
        name: name.to_string(),
        version: String::new(),
        publisher: String::new(),
        description: String::new(),
        homepage: None,
        icon: None,
        categories: vec![],
        verified: false,
        experimental: false,
        auth: None,
        settings: vec![],
        mcp: vec![],
        skills: vec![],
        provider: None,
    }
}

fn connector_only_plugin(id: &str, name: &str) -> CorePlugin {
    CorePlugin {
        manifest: minimal_manifest(id, name),
        harness: None,
        gateway: None,
        connector: Some(Arc::new(NoopConnector)),
        source: PluginSource::Builtin,
    }
}

/// Mirrors what `crates/cli/src/main.rs`'s real `build_registries` wires:
/// `native`/`discord` added first (they carry host-injected config that
/// `install_builtins` deliberately skips), then every generated builtin,
/// plus one connector-only test plugin so the `plugin.<id>.enabled` branch
/// has something to exercise.
fn test_registries() -> Registries {
    let mut regs = Registries::new();
    regs.add_plugin(ryuzi_core::harness::native::native_plugin());
    regs.add_plugin(ryuzi_core::plugins::builtin::discord_plugin());
    ryuzi_core::plugins::install_builtins(&mut regs);
    regs.add_plugin(connector_only_plugin(
        "acme-test-connector",
        "Acme Test Connector",
    ));
    regs
}

fn deps_for(
    db: &Path,
    out: Arc<std::sync::Mutex<Vec<String>>>,
    errs: Arc<std::sync::Mutex<Vec<String>>>,
) -> ryuzi_cli::dispatch::Deps {
    let o = out.clone();
    let e = errs.clone();
    ryuzi_cli::dispatch::Deps {
        db_path: db.to_path_buf(),
        out: Box::new(move |s| o.lock().unwrap().push(s.to_string())),
        err: Box::new(move |s| e.lock().unwrap().push(s.to_string())),
        prompt: Box::new(|_| String::new()),
        detect_git: || ryuzi_cli::detect::Detected {
            found: true,
            version: None,
        },
        build_registries: Box::new(|| Ok(test_registries())),
    }
}

fn run(db: &Path, args: &[&str]) -> (u8, Vec<String>, Vec<String>) {
    let out = Arc::new(std::sync::Mutex::new(Vec::new()));
    let errs = Arc::new(std::sync::Mutex::new(Vec::new()));
    let mut deps = deps_for(db, out.clone(), errs.clone());
    let code =
        ryuzi_cli::dispatch::run_cli(args.iter().map(|s| s.to_string()).collect(), &mut deps);
    let o = out.lock().unwrap().clone();
    let e = errs.lock().unwrap().clone();
    (code, o, e)
}

#[test]
fn list_shows_a_known_provider_enabled_and_verified() {
    let tmp = tempfile::tempdir().unwrap();
    let db = tmp.path().join("t.sqlite");
    let (code, out, _) = run(&db, &["plugins", "list"]);
    assert_eq!(code, 0);

    let line = out
        .iter()
        .find(|l| l.starts_with("anthropic\t"))
        .expect("anthropic line present");
    let fields: Vec<&str> = line.split('\t').collect();
    assert_eq!(fields.len(), 5);
    assert_eq!(fields[0], "anthropic");
    assert_eq!(fields[1], "Anthropic");
    assert_eq!(fields[2], "model-provider,api-key");
    // manifest-only plugin (no harness/gateway/connector) is always enabled.
    assert_eq!(fields[3], "enabled");
    assert_eq!(fields[4], "verified");
}

#[test]
fn info_anthropic_prints_categories_and_models_meta() {
    let tmp = tempfile::tempdir().unwrap();
    let db = tmp.path().join("t.sqlite");
    let (code, out, _) = run(&db, &["plugins", "info", "anthropic"]);
    assert_eq!(code, 0);

    let text = out.join("\n");
    assert!(text.contains("id: anthropic"));
    assert!(text.contains("categories: model-provider,api-key"));
    assert!(text.contains("status: verified"));
    assert!(text.contains("capabilities: provider"));
    assert!(text.contains("enabled: enabled"));
    assert!(text.contains("provider: format=anthropic"));
    assert!(text.contains("claude-opus-4-5(default)"));
}

#[test]
fn enable_then_disable_connector_only_plugin_flips_setting() {
    let tmp = tempfile::tempdir().unwrap();
    let db = tmp.path().join("t.sqlite");

    let (code, out, _) = run(&db, &["plugins", "enable", "acme-test-connector"]);
    assert_eq!(code, 0);
    assert_eq!(
        out.last().map(String::as_str),
        Some("enabled acme-test-connector")
    );
    let (_, out, _) = run(
        &db,
        &[
            "config",
            "get",
            "--reveal",
            "plugin.acme-test-connector.enabled",
        ],
    );
    assert_eq!(out.last().map(String::as_str), Some("true"));

    let (code, out, _) = run(&db, &["plugins", "disable", "acme-test-connector"]);
    assert_eq!(code, 0);
    assert_eq!(
        out.last().map(String::as_str),
        Some("disabled acme-test-connector")
    );
    let (_, out, _) = run(
        &db,
        &[
            "config",
            "get",
            "--reveal",
            "plugin.acme-test-connector.enabled",
        ],
    );
    assert_eq!(out.last().map(String::as_str), Some("false"));
}

#[test]
fn disable_discord_removes_from_seeded_enabled_gateways() {
    let tmp = tempfile::tempdir().unwrap();
    let db = tmp.path().join("t.sqlite");
    // Sanity: a fresh store seeds enabled_gateways = "discord".
    let (_, out, _) = run(&db, &["config", "get", "enabled_gateways"]);
    assert_eq!(out.last().map(String::as_str), Some("discord"));

    let (code, out, _) = run(&db, &["plugins", "disable", "discord"]);
    assert_eq!(code, 0);
    assert_eq!(out.last().map(String::as_str), Some("disabled discord"));
    let (_, out, _) = run(&db, &["config", "get", "enabled_gateways"]);
    assert_eq!(out.last().map(String::as_str), Some(""));
}

#[test]
fn unknown_plugin_id_exits_1() {
    let tmp = tempfile::tempdir().unwrap();
    let db = tmp.path().join("t.sqlite");
    for args in [
        vec!["plugins", "info", "nope"],
        vec!["plugins", "enable", "nope"],
        vec!["plugins", "disable", "nope"],
    ] {
        let (code, _, errs) = run(&db, &args);
        assert_eq!(code, 1);
        assert_eq!(
            errs.last().map(String::as_str),
            Some("unknown plugin: nope")
        );
    }
}

#[test]
fn toggling_a_manifest_only_or_experimental_plugin_errors_instead_of_no_opping() {
    let tmp = tempfile::tempdir().unwrap();
    let db = tmp.path().join("t.sqlite");

    // `anthropic` is a manifest-only provider plugin (no harness/gateway/
    // connector capability) — `is_enabled` always reports it enabled, so
    // toggling it must error rather than silently no-op (see
    // `ryuzi_core::plugins::toggle_enabled`'s doc).
    let (code, _, errs) = run(&db, &["plugins", "disable", "anthropic"]);
    assert_eq!(code, 1);
    assert_eq!(
        errs.last().map(String::as_str),
        Some("anthropic is always available")
    );

    // `zep` is one of the catalog's docs-only experimental entries —
    // `is_enabled` always reports it disabled, so toggling it must error too.
    let (code, _, errs) = run(&db, &["plugins", "enable", "zep"]);
    assert_eq!(code, 1);
    assert_eq!(
        errs.last().map(String::as_str),
        Some("zep is experimental — nothing to enable")
    );
}

/// Regression test for the real binary's composition root
/// (`crates/cli/src/main.rs`'s `build_registries`), which historically wired
/// `native`/`claude-code` but not `discord` — so `ryuzi plugins disable
/// discord` reported "unknown plugin: discord" against a real build even
/// though a fresh store seeds `enabled_gateways = "discord"`. Spawns the
/// actual compiled `ryuzi` binary (unlike every other test in this file,
/// which exercises `dispatch::run_cli` in-process against `test_registries`)
/// so a regression in `main.rs` itself — not just the fixture mirroring it —
/// would be caught.
#[test]
fn real_binary_registers_and_disables_the_discord_plugin() {
    let tmp = tempfile::tempdir().unwrap();
    assert_cmd::Command::cargo_bin("ryuzi")
        .unwrap()
        .args(["plugins", "disable", "discord"])
        .env("XDG_DATA_HOME", tmp.path())
        .env("HOME", tmp.path())
        .assert()
        .success()
        .stdout(predicate::str::contains("disabled discord"));
}

#[test]
fn enable_native_errors_because_it_is_always_enabled() {
    let tmp = tempfile::tempdir().unwrap();
    let db = tmp.path().join("t.sqlite");
    let (code, _out, errs) = run(&db, &["plugins", "enable", "native"]);
    assert_ne!(code, 0);
    assert!(
        errs.iter().any(|l| l.contains("always enabled")),
        "stderr: {errs:?}"
    );
}

#[test]
fn bare_plugins_and_unknown_subcommand_exit_2() {
    let tmp = tempfile::tempdir().unwrap();
    let db = tmp.path().join("t.sqlite");
    let (c1, _, e1) = run(&db, &["plugins"]);
    let (c2, _, e2) = run(&db, &["plugins", "bogus"]);
    assert_eq!((c1, c2), (2, 2));
    assert_eq!(
        e1.last().map(String::as_str),
        Some("usage: ryuzi plugins <list|info|enable|disable> ...")
    );
    assert_eq!(
        e2.last().map(String::as_str),
        Some("usage: ryuzi plugins <list|info|enable|disable> ...")
    );
}
