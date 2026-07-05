//! Stage a canary binary downloaded from GitHub releases, verified by
//! checksum, with an injectable host for the tar-extract and file-write
//! effects so staging is fully testable.

use super::asset::{asset_name, asset_url, checksums_url, verify_checksum, Platform};
use super::check::UpdateHttp;
use anyhow::Context;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, PartialEq)]
pub struct StageOpts {
    pub repo: String,
    pub tag: String,
    pub version: String,
    pub install_path: PathBuf,
}

pub trait StageHost: Send + Sync {
    fn extract_ryuzi(&self, tar_path: &Path, dest_dir: &Path) -> anyhow::Result<Vec<u8>>;
    fn write_file(&self, path: &Path, bytes: &[u8], mode: u32) -> anyhow::Result<()>;
}

#[derive(Debug, Clone, PartialEq)]
pub struct StageResult {
    pub ok: bool,
    pub canary_path: Option<PathBuf>,
    pub error: Option<String>,
}

/// BLOCKING — call via spawn_blocking. Never panics; every failure returns
/// StageResult{ok:false, error:Some(..)} so callers always get a result to
/// report instead of an unwound stack.
pub fn stage_canary(
    opts: &StageOpts,
    platform: Platform,
    tmp_dir: &Path,
    http: &dyn UpdateHttp,
    host: &dyn StageHost,
) -> StageResult {
    let fail = |m: String| StageResult {
        ok: false,
        canary_path: None,
        error: Some(m),
    };

    let Some(name) = asset_name(&opts.version, platform) else {
        return fail("unsupported platform".into());
    };

    let asset = match http.get(&asset_url(&opts.repo, &opts.tag, &name)) {
        Ok(r) => r,
        Err(e) => return fail(e.to_string()),
    };
    if !(200..300).contains(&asset.status) {
        return fail(format!("asset download failed: HTTP {}", asset.status));
    }

    let sums = match http.get(&checksums_url(&opts.repo, &opts.tag)) {
        Ok(r) => r,
        Err(e) => return fail(e.to_string()),
    };
    if !(200..300).contains(&sums.status) {
        return fail(format!("checksums download failed: HTTP {}", sums.status));
    }

    let checksums = String::from_utf8_lossy(&sums.body);
    if !verify_checksum(&asset.body, &name, &checksums) {
        return fail(format!("checksum verification failed for {name}"));
    }

    let tar_path = tmp_dir.join(&name);
    if let Err(e) = host.write_file(&tar_path, &asset.body, 0o600) {
        return fail(e.to_string());
    }

    let ryuzi_bytes = match host.extract_ryuzi(&tar_path, tmp_dir) {
        Ok(b) => b,
        Err(e) => return fail(e.to_string()),
    };

    let canary_path = opts
        .install_path
        .parent()
        .unwrap_or(Path::new("."))
        .join(".ryuzi.canary");
    if let Err(e) = host.write_file(&canary_path, &ryuzi_bytes, 0o755) {
        return fail(e.to_string());
    }

    StageResult {
        ok: true,
        canary_path: Some(canary_path),
        error: None,
    }
}

/// Production host: `tar -xzf {tar} -C {dest}` then read `{dest}/ryuzi`;
/// write_file = fs::write + set_permissions(mode) on unix.
pub struct TarStageHost;

impl StageHost for TarStageHost {
    fn extract_ryuzi(&self, tar_path: &Path, dest_dir: &Path) -> anyhow::Result<Vec<u8>> {
        let out = std::process::Command::new("tar")
            .args(["-xzf"])
            .arg(tar_path)
            .arg("-C")
            .arg(dest_dir)
            .output()
            .context("tar command failed")?;

        if !out.status.success() {
            let stderr = String::from_utf8_lossy(&out.stderr);
            let msg: String = stderr.chars().take(200).collect();
            anyhow::bail!("tar failed: {msg}");
        }

        std::fs::read(dest_dir.join("ryuzi")).context("read ryuzi from tar extraction")
    }

    fn write_file(&self, path: &Path, bytes: &[u8], mode: u32) -> anyhow::Result<()> {
        std::fs::write(path, bytes)?;
        #[cfg(unix)]
        {
            use std::fs::Permissions;
            use std::os::unix::fs::PermissionsExt;
            let perms = Permissions::from_mode(mode);
            std::fs::set_permissions(path, perms)?;
        }
        // Windows has no POSIX file mode, so `mode` is unused there.
        #[cfg(not(unix))]
        let _ = mode;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::update::asset::sha256_hex;
    use crate::update::check::HttpResponse;

    const PLATFORM: Platform = Platform {
        os: "linux",
        arch: "x86_64",
        musl: false,
    };
    const NAME: &str = "ryuzi-0.3.0-x86_64-unknown-linux-gnu.tar.gz";

    struct FakeHttp {
        asset: Vec<u8>,
        checksums: String,
        asset_status: u16,
        checksums_status: u16,
    }

    impl UpdateHttp for FakeHttp {
        fn get(&self, url: &str) -> anyhow::Result<HttpResponse> {
            if url.ends_with("checksums.txt") {
                Ok(HttpResponse {
                    status: self.checksums_status,
                    body: self.checksums.clone().into_bytes(),
                })
            } else {
                Ok(HttpResponse {
                    status: self.asset_status,
                    body: self.asset.clone(),
                })
            }
        }
    }

    struct RecordingHost {
        writes: std::sync::Mutex<Vec<(PathBuf, u32)>>,
    }

    impl StageHost for RecordingHost {
        fn extract_ryuzi(&self, _tar: &Path, _dest: &Path) -> anyhow::Result<Vec<u8>> {
            Ok(b"#!/fake/ryuzi binary\n".to_vec())
        }
        fn write_file(&self, path: &Path, _bytes: &[u8], mode: u32) -> anyhow::Result<()> {
            self.writes.lock().unwrap().push((path.to_path_buf(), mode));
            Ok(())
        }
    }

    fn rig(
        asset_status: u16,
        checksums_status: u16,
        good_sum: bool,
    ) -> (FakeHttp, RecordingHost, StageOpts) {
        let asset = b"tarball".to_vec();
        let sum = if good_sum {
            sha256_hex(&asset)
        } else {
            "deadbeef".into()
        };
        (
            FakeHttp {
                asset,
                checksums: format!("{sum}  {NAME}\n"),
                asset_status,
                checksums_status,
            },
            RecordingHost {
                writes: std::sync::Mutex::new(vec![]),
            },
            StageOpts {
                repo: "o/r".into(),
                tag: "v0.3.0".into(),
                version: "0.3.0".into(),
                install_path: PathBuf::from("/home/me/.local/bin/ryuzi"),
            },
        )
    }

    #[test]
    fn stage_downloads_verifies_extracts_and_writes_canary_0755() {
        let (http, host, opts) = rig(200, 200, true);
        let res = stage_canary(&opts, PLATFORM, Path::new("/tmp/ryuzi-stage"), &http, &host);
        assert!(res.ok, "{:?}", res.error);
        assert_eq!(
            res.canary_path.as_deref(),
            Some(Path::new("/home/me/.local/bin/.ryuzi.canary"))
        );
        assert_eq!(
            *host.writes.lock().unwrap(),
            vec![
                (PathBuf::from(format!("/tmp/ryuzi-stage/{NAME}")), 0o600),
                (PathBuf::from("/home/me/.local/bin/.ryuzi.canary"), 0o755),
            ]
        );
    }

    #[test]
    fn checksum_mismatch_fails_with_no_writes() {
        let (http, host, opts) = rig(200, 200, false);
        let res = stage_canary(&opts, PLATFORM, Path::new("/tmp/x"), &http, &host);
        assert!(!res.ok);
        assert!(res.error.unwrap().to_lowercase().contains("checksum"));
        assert!(host.writes.lock().unwrap().is_empty());
    }

    #[test]
    fn asset_404_fails_with_no_writes() {
        let (http, host, opts) = rig(404, 200, true);
        let res = stage_canary(&opts, PLATFORM, Path::new("/tmp/x"), &http, &host);
        assert!(!res.ok);
        assert!(res
            .error
            .unwrap()
            .contains("asset download failed: HTTP 404"));
        assert!(host.writes.lock().unwrap().is_empty());
    }

    #[test]
    fn checksums_403_fails_with_no_writes() {
        let (http, host, opts) = rig(200, 403, true);
        let res = stage_canary(&opts, PLATFORM, Path::new("/tmp/x"), &http, &host);
        assert!(!res.ok);
        assert!(res
            .error
            .unwrap()
            .contains("checksums download failed: HTTP 403"));
        assert!(host.writes.lock().unwrap().is_empty());
    }

    #[test]
    fn tar_stage_host_extracts_a_real_tarball() {
        // build a real tar.gz in a tempdir containing a `ryuzi` file, then
        // assert TarStageHost::extract_ryuzi returns its bytes and a bad
        // tarball errs with "tar failed:".
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("ryuzi"), b"binary-bytes").unwrap();
        let tar = dir.path().join("a.tar.gz");
        let ok = std::process::Command::new("tar")
            .args(["-czf"])
            .arg(&tar)
            .args(["-C"])
            .arg(dir.path())
            .arg("ryuzi")
            .status()
            .unwrap()
            .success();
        assert!(ok);
        let out = tempfile::tempdir().unwrap();
        let bytes = TarStageHost.extract_ryuzi(&tar, out.path()).unwrap();
        assert_eq!(bytes, b"binary-bytes");
        let bad = dir.path().join("bad.tar.gz");
        std::fs::write(&bad, b"not a tarball").unwrap();
        let err = TarStageHost
            .extract_ryuzi(&bad, out.path())
            .unwrap_err()
            .to_string();
        assert!(err.starts_with("tar failed:"), "{err}");
    }
}
