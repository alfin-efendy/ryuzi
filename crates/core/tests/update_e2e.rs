//! Offline end-to-end update flow (design §9): local fake release server →
//! stage (real HTTP + tar + sha256) → applier state machine with real
//! update.json handoffs and real renames → swap, and rollback.
use ryuzi_core::update::*;
use std::io::{Read, Write};
use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

/// One-thread HTTP/1.0 server: serves (path → bytes) routes with 200s, 404 otherwise.
fn serve(routes: Vec<(String, Vec<u8>)>) -> (String, std::thread::JoinHandle<()>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let handle = std::thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(mut s) = stream else { break };
            let mut buf = [0u8; 4096];
            let n = s.read(&mut buf).unwrap_or(0);
            let req = String::from_utf8_lossy(&buf[..n]);
            let path = req.split_whitespace().nth(1).unwrap_or("/").to_string();
            if path == "/quit" {
                let _ = s.write_all(b"HTTP/1.0 200 OK\r\nContent-Length: 0\r\n\r\n");
                break;
            }
            match routes.iter().find(|(p, _)| *p == path) {
                Some((_, body)) => {
                    let _ = s.write_all(
                        format!("HTTP/1.0 200 OK\r\nContent-Length: {}\r\n\r\n", body.len())
                            .as_bytes(),
                    );
                    let _ = s.write_all(body);
                }
                None => {
                    let _ = s.write_all(b"HTTP/1.0 404 Not Found\r\nContent-Length: 0\r\n\r\n");
                }
            }
        }
    });
    (base, handle)
}

/// UpdateHttp that rewrites github.com URLs onto the local server, then
/// delegates to the REAL UreqHttp — stage_canary's URL construction stays
/// fully exercised.
struct RewriteHttp {
    base: String,
}
impl check::UpdateHttp for RewriteHttp {
    fn get(&self, url: &str) -> anyhow::Result<check::HttpResponse> {
        let rewritten = url.replace("https://github.com", &self.base);
        check::UreqHttp.get(&rewritten)
    }
}

/// Build a real release: tar.gz containing `ryuzi` (the "new binary"), plus
/// checksums.txt for the current platform's asset name.
fn fake_release(version: &str, new_binary: &[u8]) -> (Platform, String, Vec<u8>, String) {
    let platform = detect_platform().expect("supported CI platform");
    let name = asset_name(version, platform).unwrap();
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("ryuzi"), new_binary).unwrap();
    let tar_path = dir.path().join(&name);
    assert!(std::process::Command::new("tar")
        .args(["-czf"])
        .arg(&tar_path)
        .args(["-C"])
        .arg(dir.path())
        .arg("ryuzi")
        .status()
        .unwrap()
        .success());
    let tar_bytes = std::fs::read(&tar_path).unwrap();
    let checksums = format!("{}  {}\n", sha256_hex(&tar_bytes), name);
    (platform, name, tar_bytes, checksums)
}

struct E2eHost {
    dir: PathBuf,          // handoff dir
    install_path: PathBuf, // fake old binary
    platform: Platform,
    tmp_dir: PathBuf,
    http_base: String,
    repo: String,
    tag: String,
    version: String,
    /// The scripted "canary process": promote=true → speaks probing→healthy,
    /// then answers the promote signal with promoted; false → healthy forever.
    canary_promotes: bool,
    canary_threads: Mutex<Vec<std::thread::JoinHandle<()>>>,
    log: Mutex<Vec<String>>,
}

#[async_trait::async_trait]
impl ApplierHost for E2eHost {
    async fn stage(&self) -> StageResult {
        let opts = StageOpts {
            repo: self.repo.clone(),
            tag: self.tag.clone(),
            version: self.version.clone(),
            install_path: self.install_path.clone(),
        };
        let (platform, tmp, base) = (self.platform, self.tmp_dir.clone(), self.http_base.clone());
        tokio::task::spawn_blocking(move || {
            stage_canary(&opts, platform, &tmp, &RewriteHttp { base }, &TarStageHost)
        })
        .await
        .unwrap()
    }
    fn spawn_canary(&self, canary_path: &Path) -> anyhow::Result<i32> {
        assert!(
            canary_path.exists(),
            "canary staged next to the install path"
        );
        let (dir, version, promotes) =
            (self.dir.clone(), self.version.clone(), self.canary_promotes);
        let t = std::thread::spawn(move || {
            let h = |phase| Handoff {
                phase,
                pid: 4242,
                version: version.clone(),
                detail: None,
            };
            let _ = write_handoff(&dir, &h(HandoffPhase::Probing));
            let _ = write_handoff(&dir, &h(HandoffPhase::Healthy));
            if promotes {
                for _ in 0..100 {
                    if read_handoff(&dir).map(|x| x.phase) == Some(HandoffPhase::Promote) {
                        let _ = write_handoff(&dir, &h(HandoffPhase::Promoted));
                        return;
                    }
                    std::thread::sleep(std::time::Duration::from_millis(20));
                }
            }
        });
        self.canary_threads.lock().unwrap().push(t);
        Ok(4242)
    }
    fn read_handoff(&self) -> Option<Handoff> {
        read_handoff(&self.dir)
    }
    fn write_handoff(&self, h: &Handoff) {
        let _ = write_handoff(&self.dir, h);
    }
    fn clear_handoff(&self) {
        clear_handoff(&self.dir);
    }
    async fn drain(&self, _ms: u64) {}
    fn backup(&self) -> anyhow::Result<()> {
        let mut bak = self.install_path.as_os_str().to_owned();
        bak.push(".bak");
        Ok(std::fs::rename(&self.install_path, PathBuf::from(bak))?)
    }
    fn swap(&self) -> anyhow::Result<()> {
        let canary = self.install_path.parent().unwrap().join(".ryuzi.canary");
        Ok(std::fs::rename(canary, &self.install_path)?)
    }
    fn restore(&self) -> anyhow::Result<()> {
        let mut bak = self.install_path.as_os_str().to_owned();
        bak.push(".bak");
        Ok(std::fs::rename(PathBuf::from(bak), &self.install_path)?)
    }
    fn kill_canary(&self, _pid: i32) {}
    async fn stop_gateways(&self) {}
    fn now(&self) -> i64 {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_millis() as i64
    }
    async fn sleep_ms(&self, ms: u64) {
        tokio::time::sleep(std::time::Duration::from_millis(ms)).await;
    }
    fn log(&self, m: &str) {
        self.log.lock().unwrap().push(m.to_string());
    }
}

fn setup(
    promotes: bool,
) -> (
    E2eHost,
    ApplierCfg,
    tempfile::TempDir,
    String,
    std::thread::JoinHandle<()>,
) {
    let version = "9.9.9";
    let (platform, name, tar_bytes, checksums) = fake_release(version, b"NEW-BINARY");
    let (base, server) = serve(vec![
        (format!("/o/r/releases/download/v9.9.9/{name}"), tar_bytes),
        (
            "/o/r/releases/download/v9.9.9/checksums.txt".to_string(),
            checksums.into_bytes(),
        ),
    ]);
    let root = tempfile::tempdir().unwrap();
    let bin_dir = root.path().join("bin");
    std::fs::create_dir_all(&bin_dir).unwrap();
    let install_path = bin_dir.join("ryuzi");
    std::fs::write(&install_path, b"OLD-BINARY").unwrap();
    let tmp_dir = root.path().join("stage-tmp");
    std::fs::create_dir_all(&tmp_dir).unwrap();
    let host = E2eHost {
        dir: root.path().to_path_buf(),
        install_path,
        platform,
        tmp_dir,
        http_base: base.clone(),
        repo: "o/r".into(),
        tag: "v9.9.9".into(),
        version: version.into(),
        canary_promotes: promotes,
        canary_threads: Mutex::new(vec![]),
        log: Mutex::new(vec![]),
    };
    let cfg = ApplierCfg {
        version: version.into(),
        drain_timeout_ms: 100,
        canary_timeout_ms: if promotes { 5_000 } else { 500 },
    };
    (host, cfg, root, base, server)
}

fn shutdown(base: &str, server: std::thread::JoinHandle<()>) {
    let _ = ureq::get(&format!("{base}/quit")).call();
    let _ = server.join();
}

#[tokio::test]
async fn e2e_stage_canary_swap_promote() {
    let (host, cfg, root, base, server) = setup(true);
    let outcome = apply_update(&cfg, &host).await.unwrap();
    assert_eq!(outcome, ApplyOutcome::Promoted);
    let installed = std::fs::read(root.path().join("bin/ryuzi")).unwrap();
    assert_eq!(
        installed, b"NEW-BINARY",
        "swap must land the staged canary at the install path"
    );
    let bak = std::fs::read(root.path().join("bin/ryuzi.bak")).unwrap();
    assert_eq!(bak, b"OLD-BINARY", "backup must hold the previous binary");
    assert_eq!(
        read_handoff(root.path()),
        None,
        "handoff cleared after promote"
    );
    shutdown(&base, server);
}

#[tokio::test]
async fn e2e_missing_promote_rolls_back_to_the_old_binary() {
    let (host, cfg, root, base, server) = setup(false);
    let outcome = apply_update(&cfg, &host).await.unwrap();
    assert_eq!(outcome, ApplyOutcome::RolledBack);
    let installed = std::fs::read(root.path().join("bin/ryuzi")).unwrap();
    assert_eq!(
        installed, b"OLD-BINARY",
        "rollback must restore the previous binary"
    );
    assert!(
        !root.path().join("bin/ryuzi.bak").exists(),
        "restore consumed the backup"
    );
    assert_eq!(read_handoff(root.path()), None);
    shutdown(&base, server);
}
