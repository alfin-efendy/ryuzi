use std::path::{Path, PathBuf};
use std::sync::Arc;

use ryuzi_core::daemon_status::read_status;
use ryuzi_core::pairing::mint_code;
use ryuzi_core::paths::now_ms;
use ryuzi_core::settings::SettingsStore;

use crate::dispatch::Deps;

/// 10-minute TTL for a freshly minted pairing code — the plaintext code is
/// the caller's one and only chance to see it (see `pairing::mint_code`).
const PAIR_CODE_TTL_MS: i64 = 600_000;

pub fn cmd_pair(args: &[String], deps: &mut Deps) -> u8 {
    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .expect("tokio runtime");
    rt.block_on(pair_inner(args, deps))
}

async fn pair_inner(args: &[String], deps: &mut Deps) -> u8 {
    if args.iter().any(|a| a == "--list") {
        return list(deps).await;
    }
    if let Some(pos) = args.iter().position(|a| a == "--revoke") {
        let Some(id) = args.get(pos + 1) else {
            (deps.err)("usage: ryuzi pair --revoke <id>");
            return 1;
        };
        return revoke(deps, id).await;
    }
    mint(deps).await
}

/// Bare `ryuzi pair`: mint a fresh single-use code and print everything a
/// human needs to complete pairing from the remote Cockpit — the code
/// itself, where to reach the daemon's `/pair` route, and the TLS
/// fingerprint to pin (if any).
async fn mint(deps: &mut Deps) -> u8 {
    let dir: PathBuf = deps
        .db_path
        .parent()
        .map(Path::to_path_buf)
        .unwrap_or_else(|| PathBuf::from("."));

    let store = match crate::db::open_store(deps).await {
        Ok(s) => s,
        Err(e) => {
            (deps.err)(&format!("✗ {e}"));
            return 1;
        }
    };

    // Written to the SAME sqlite store the running daemon reads (both use
    // `db_path().parent()` as the state dir), so the daemon can validate it
    // when the device redeems it via `POST /pair`.
    let code = match mint_code(&store, PAIR_CODE_TTL_MS, now_ms()).await {
        Ok(c) => c,
        Err(e) => {
            (deps.err)(&format!("✗ {e}"));
            return 1;
        }
    };

    (deps.out)(&format!("pairing code: {code}"));
    (deps.out)("expires in 10 minutes, single use");

    match read_status(&dir).and_then(|s| s.port.map(|port| (s, port))) {
        Some((status, port)) => {
            let scheme = status.scheme.as_deref().unwrap_or("http");
            let host = status.host.as_deref().unwrap_or("127.0.0.1");
            (deps.out)(&format!("address: {scheme}://{host}:{port}"));
            match &status.fingerprint {
                Some(fp) => (deps.out)(&format!("fingerprint: {fp}")),
                None => (deps.out)("fingerprint: (no TLS — loopback/plain http)"),
            }
        }
        None => {
            (deps.out)(
                "note: daemon does not appear to be running — it must be running to serve /pair",
            );
            let settings = SettingsStore::new(Arc::new(store));
            let listen_addr = settings
                .get("listen_addr")
                .await
                .ok()
                .flatten()
                .unwrap_or_else(|| "127.0.0.1".to_string());
            let control_port: u16 = settings
                .get("control_port")
                .await
                .ok()
                .flatten()
                .and_then(|v| v.parse().ok())
                .unwrap_or(crate::daemon_cmd::DEFAULT_CONTROL_PORT);
            (deps.out)(&format!(
                "address (once started): {listen_addr}:{control_port}"
            ));
            (deps.out)("fingerprint: (no TLS — loopback/plain http)");
        }
    }

    0
}

async fn list(deps: &mut Deps) -> u8 {
    let store = match crate::db::open_store(deps).await {
        Ok(s) => s,
        Err(e) => {
            (deps.err)(&format!("✗ {e}"));
            return 1;
        }
    };
    let devices = match store.list_devices().await {
        Ok(d) => d,
        Err(e) => {
            (deps.err)(&format!("✗ {e}"));
            return 1;
        }
    };
    if devices.is_empty() {
        (deps.out)("no paired devices");
        return 0;
    }
    for d in devices {
        let revoked = if d.revoked { "  [revoked]" } else { "" };
        (deps.out)(&format!(
            "{}  {}  created={}{revoked}",
            d.id, d.name, d.created_at
        ));
    }
    0
}

async fn revoke(deps: &mut Deps, id: &str) -> u8 {
    let store = match crate::db::open_store(deps).await {
        Ok(s) => s,
        Err(e) => {
            (deps.err)(&format!("✗ {e}"));
            return 1;
        }
    };
    match store.revoke_device(id).await {
        Ok(true) => {
            (deps.out)(&format!("revoked {id}"));
            0
        }
        Ok(false) => {
            (deps.err)(&format!("no such device: {id}"));
            1
        }
        Err(e) => {
            (deps.err)(&format!("✗ {e}"));
            1
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;
    use std::rc::Rc;

    #[allow(clippy::type_complexity)]
    fn capture() -> (Box<dyn FnMut(&str)>, Rc<RefCell<Vec<String>>>) {
        let lines = Rc::new(RefCell::new(Vec::new()));
        let sink = lines.clone();
        (
            Box::new(move |s: &str| sink.borrow_mut().push(s.to_string())),
            lines,
        )
    }

    #[allow(clippy::type_complexity)]
    fn test_deps(
        db: &std::path::Path,
    ) -> (Deps, Rc<RefCell<Vec<String>>>, Rc<RefCell<Vec<String>>>) {
        let (out, out_lines) = capture();
        let (err, err_lines) = capture();
        let deps = Deps {
            db_path: db.to_path_buf(),
            out,
            err,
            prompt: Box::new(|_q| String::new()),
            detect_git: || crate::detect::Detected {
                found: true,
                version: None,
            },
        };
        (deps, out_lines, err_lines)
    }

    #[test]
    fn bare_pair_mints_a_code_that_redeems_and_prints_an_address_hint() {
        let tmp = tempfile::tempdir().unwrap();
        let db = tmp.path().join("d.sqlite");
        let (mut deps, out, _err) = test_deps(&db);

        let code = cmd_pair(&[], &mut deps);
        assert_eq!(code, 0, "lines so far: {:?}", out.borrow());

        let lines = out.borrow().clone();
        let code_line = lines
            .iter()
            .find(|l| l.starts_with("pairing code:"))
            .expect("prints a code line");
        let minted = code_line
            .trim_start_matches("pairing code:")
            .trim()
            .to_string();
        assert!(
            lines
                .iter()
                .any(|l| l.contains("not running") || l.starts_with("address")),
            "prints an address hint either way: {lines:?}"
        );

        // Follow-up consume proves the code was persisted in the SAME store
        // the running daemon would read (db_path().parent()).
        let rt = tokio::runtime::Runtime::new().unwrap();
        let redeemed = rt.block_on(async {
            let store = ryuzi_core::Store::open(&db).await.unwrap();
            ryuzi_core::pairing::redeem(&store, &minted, "test-device", ryuzi_core::paths::now_ms())
                .await
                .unwrap()
        });
        assert!(redeemed.is_some(), "minted code should redeem a token");
    }

    #[test]
    fn list_prints_seeded_devices() {
        let tmp = tempfile::tempdir().unwrap();
        let db = tmp.path().join("d.sqlite");
        {
            let rt = tokio::runtime::Runtime::new().unwrap();
            rt.block_on(async {
                let store = ryuzi_core::Store::open(&db).await.unwrap();
                store
                    .insert_device("dev-1", "alfin-laptop", "hash-abc")
                    .await
                    .unwrap();
            });
        }
        let (mut deps, out, _err) = test_deps(&db);
        let code = cmd_pair(&["--list".to_string()], &mut deps);
        assert_eq!(code, 0);
        assert!(out.borrow().iter().any(|l| l.contains("alfin-laptop")));
    }

    #[test]
    fn revoke_known_device_succeeds_and_unknown_device_fails() {
        let tmp = tempfile::tempdir().unwrap();
        let db = tmp.path().join("d.sqlite");
        {
            let rt = tokio::runtime::Runtime::new().unwrap();
            rt.block_on(async {
                let store = ryuzi_core::Store::open(&db).await.unwrap();
                store
                    .insert_device("dev-1", "alfin-laptop", "hash-abc")
                    .await
                    .unwrap();
            });
        }

        let (mut deps, out, _err) = test_deps(&db);
        let code = cmd_pair(&["--revoke".to_string(), "dev-1".to_string()], &mut deps);
        assert_eq!(code, 0);
        assert!(out.borrow().iter().any(|l| l.contains("revoked dev-1")));

        let (mut deps2, _out2, err2) = test_deps(&db);
        let code2 = cmd_pair(&["--revoke".to_string(), "no-such".to_string()], &mut deps2);
        assert_eq!(code2, 1);
        assert!(err2
            .borrow()
            .iter()
            .any(|l| l.contains("no such device: no-such")));
    }
}
