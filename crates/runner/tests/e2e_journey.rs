//! End-to-end journey over the compiled `ryuzi` binary: config persistence
//! and the daemon lifecycle share one isolated HOME, the way a real install
//! does. Complements the narrower per-command tests (cli.rs, config.rs,
//! daemon.rs) — this is the cross-command regression net.
#![cfg(unix)]

use std::process::Stdio;
use std::sync::Arc;
use std::time::{Duration, Instant};

use assert_cmd::Command;
use predicates::prelude::*;
use ryuzi_core::daemon_status::{read_status, send_sigterm, DaemonFileState};
use ryuzi_core::settings::SettingsStore;
use ryuzi_core::Store;
use serial_test::serial;

fn bin(data_home: &std::path::Path, home: &std::path::Path) -> Command {
    let mut c = Command::cargo_bin("ryuzi").unwrap();
    c.env("XDG_DATA_HOME", data_home).env("HOME", home);
    c
}

#[test]
#[serial]
fn config_survives_a_full_daemon_lifecycle() {
    let tmp = tempfile::tempdir().unwrap();
    let data_home = tmp.path().join("data");
    let home = tmp.path().to_path_buf();

    // 1. config set/get round-trips through the real binary.
    bin(&data_home, &home)
        .args(["config", "set", "default_effort", "high"])
        .assert()
        .success();
    bin(&data_home, &home)
        .args(["config", "get", "default_effort"])
        .assert()
        .success()
        .stdout(predicate::str::contains("high"));

    // 2. Seed zero-gateway settings so the daemon never touches the network
    //    (same seeding as crates/runner/tests/daemon.rs).
    std::env::set_var("XDG_DATA_HOME", &data_home);
    std::env::set_var("HOME", &home);
    let db_path = ryuzi_core::paths::db_path();
    let data_dir = db_path.parent().unwrap().to_path_buf();
    {
        let rt = tokio::runtime::Runtime::new().unwrap();
        rt.block_on(async {
            let store = Store::open(&db_path).await.unwrap();
            let settings = SettingsStore::new(Arc::new(store));
            settings.set("enabled_gateways", "").await.unwrap();
            settings.set("auto_update", "off").await.unwrap();
        });
    }

    // 3. Daemon reaches running, then exits cleanly on SIGTERM.
    let mut child = std::process::Command::new(assert_cmd::cargo::cargo_bin("ryuzi"))
        .arg("__daemon")
        .env("XDG_DATA_HOME", &data_home)
        .env("HOME", &home)
        .stdin(Stdio::null())
        .spawn()
        .expect("failed to spawn `ryuzi __daemon`");
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        if let Some(status) = read_status(&data_dir) {
            if matches!(status.state, DaemonFileState::Running) {
                break;
            }
        }
        if let Some(code) = child.try_wait().unwrap() {
            panic!("daemon exited early with {code:?}");
        }
        if Instant::now() >= deadline {
            let _ = child.kill();
            panic!("daemon never reached state \"running\" within 10s");
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    send_sigterm(child.id() as i32);
    let deadline = Instant::now() + Duration::from_secs(10);
    let exit = loop {
        if let Some(s) = child.try_wait().unwrap() {
            break s;
        }
        if Instant::now() >= deadline {
            let _ = child.kill();
            panic!("daemon did not exit within 10s of SIGTERM");
        }
        std::thread::sleep(Duration::from_millis(50));
    };
    assert_eq!(exit.code(), Some(0));
    assert!(
        read_status(&data_dir).is_none(),
        "daemon.json must be removed after a clean shutdown"
    );

    // 4. The setting written before the daemon run is still there after it.
    bin(&data_home, &home)
        .args(["config", "get", "default_effort"])
        .assert()
        .success()
        .stdout(predicate::str::contains("high"));
}
