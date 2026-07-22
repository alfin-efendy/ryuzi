use std::path::Path;
use std::sync::Arc;

fn deps_for(
    db: &Path,
    out: Arc<std::sync::Mutex<Vec<String>>>,
    errs: Arc<std::sync::Mutex<Vec<String>>>,
) -> ryuzi_runner::dispatch::Deps {
    let o = out.clone();
    let e = errs.clone();
    ryuzi_runner::dispatch::Deps {
        db_path: db.to_path_buf(),
        out: Box::new(move |s| o.lock().unwrap().push(s.to_string())),
        err: Box::new(move |s| e.lock().unwrap().push(s.to_string())),
        prompt: Box::new(|_| String::new()),
        detect_git: || ryuzi_runner::detect::Detected {
            found: true,
            version: None,
        },
    }
}

fn run(db: &Path, args: &[&str]) -> (u8, Vec<String>, Vec<String>) {
    let out = Arc::new(std::sync::Mutex::new(Vec::new()));
    let errs = Arc::new(std::sync::Mutex::new(Vec::new()));
    let mut deps = deps_for(db, out.clone(), errs.clone());
    let code =
        ryuzi_runner::dispatch::run_cli(args.iter().map(|s| s.to_string()).collect(), &mut deps);
    let o = out.lock().unwrap().clone();
    let e = errs.lock().unwrap().clone();
    (code, o, e)
}

#[test]
fn set_then_get_persists_within_one_db_file() {
    let tmp = tempfile::tempdir().unwrap();
    let db = tmp.path().join("t.sqlite");
    assert_eq!(run(&db, &["config", "set", "default_effort", "high"]).0, 0);
    let (code, out, _) = run(&db, &["config", "get", "default_effort"]);
    assert_eq!(code, 0);
    assert_eq!(out.last().map(String::as_str), Some("high"));
}

#[test]
fn set_invalid_value_returns_nonzero_with_exact_message() {
    let tmp = tempfile::tempdir().unwrap();
    let db = tmp.path().join("t.sqlite");
    let (code, _, errs) = run(&db, &["config", "set", "default_perm_mode", "bogus"]);
    assert_eq!(code, 1);
    assert_eq!(
        errs.last().map(String::as_str),
        Some("default_perm_mode must be one of: default, acceptEdits, bypassPermissions")
    );
}

#[test]
fn get_redacts_secrets_unless_revealed() {
    let tmp = tempfile::tempdir().unwrap();
    let db = tmp.path().join("t.sqlite");
    // `plugin.github.token` is a registered secret plugin field (the github
    // catalog manifest's `auth.setting`) — see
    // `plugin_setting_is_recognized_validated_and_redacted` below.
    run(
        &db,
        &["config", "set", "plugin.github.token", "supersecret"],
    );
    let (_, out, _) = run(&db, &["config", "get", "plugin.github.token"]);
    assert_eq!(out.last().map(String::as_str), Some("••••••••"));
    let (_, out, _) = run(&db, &["config", "get", "--reveal", "plugin.github.token"]); // flag before key
    assert_eq!(out.last().map(String::as_str), Some("supersecret"));
    // unknown/unset key prints empty and exits 0:
    let (code, out, _) = run(&db, &["config", "get", "totally_unknown"]);
    assert_eq!(code, 0);
    assert_eq!(out.last().map(String::as_str), Some(""));
}

#[test]
fn list_shows_redaction_defaults_and_unset() {
    let tmp = tempfile::tempdir().unwrap();
    let db = tmp.path().join("t.sqlite");
    let (code, out, _) = run(&db, &["config", "list"]);
    assert_eq!(code, 0);
    let text = out.join("\n");
    assert!(text.contains("default_effort = medium (default)"));
    assert!(text.contains("default_perm_mode = default (default)"));
    assert!(text.contains("approval_timeout_ms = 300000 (default)"));
    assert!(text.contains("native_tools.version = v1 (default)"));
    assert!(text.contains("workdir_root = (unset)"));
    // No native gateway seed anymore: `enabled_gateways` is unset on a fresh db.
    assert!(text.contains("enabled_gateways = (unset)"));
    assert_eq!(out.len(), ryuzi_core::settings::all_fields().len());
    assert_eq!(out[0].split(" = ").next(), Some("workdir_root"));
}

/// Regression test: `cmd_config` used to run settings get/set/list without
/// ever populating the process-wide `plugin.*` fields registry (that table
/// is normally populated as a side effect of `Registries::add_plugin`, which
/// `ryuzi config` never calls — it opens the settings store directly instead
/// of building a full `Registries`).
/// So `config set plugin.<id>.<key> ...` failed "unknown setting" for every
/// real plugin field, and `config get` would report a registered secret as
/// non-secret (empty table → `is_secret` false) and print it unredacted.
#[test]
fn plugin_setting_is_recognized_validated_and_redacted() {
    let tmp = tempfile::tempdir().unwrap();
    let db = tmp.path().join("t.sqlite");

    // `plugin.github.token` comes from the `github` catalog manifest's
    // `[auth]` block (`auth.setting`), registered as a synthetic secret
    // `String` field by `register_plugin_fields`.
    let (code, out, _) = run(&db, &["config", "set", "plugin.github.token", "tok"]);
    assert_eq!(code, 0);
    assert_eq!(
        out.last().map(String::as_str),
        Some("set plugin.github.token")
    );

    let (code, out, _) = run(&db, &["config", "get", "plugin.github.token"]);
    assert_eq!(code, 0);
    assert_eq!(out.last().map(String::as_str), Some("••••••••"));

    let (_, out, _) = run(&db, &["config", "get", "--reveal", "plugin.github.token"]);
    assert_eq!(out.last().map(String::as_str), Some("tok"));

    // An unrecognized `plugin.*` key must still error "unknown setting",
    // same as before this fix.
    let (code, _, errs) = run(&db, &["config", "set", "plugin.nope-unknown.token", "x"]);
    assert_eq!(code, 1);
    assert_eq!(
        errs.last().map(String::as_str),
        Some("unknown setting: plugin.nope-unknown.token")
    );
}

#[test]
fn usage_strings_and_unknown_subcommand() {
    let tmp = tempfile::tempdir().unwrap();
    let db = tmp.path().join("t.sqlite");
    let (c1, _, e1) = run(&db, &["config", "get"]);
    let (c2, _, e2) = run(&db, &["config", "set", "default_effort"]);
    let (c3, _, e3) = run(&db, &["config", "bogus"]);
    assert_eq!((c1, c2, c3), (1, 1, 1));
    assert_eq!(
        e1.last().map(String::as_str),
        Some("usage: ryuzi config get <key> [--reveal]")
    );
    assert_eq!(
        e2.last().map(String::as_str),
        Some("usage: ryuzi config set <key> <value>")
    );
    assert_eq!(
        e3.last().map(String::as_str),
        Some("usage: ryuzi config <get|set|list> ...")
    );
}
