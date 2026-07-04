//! Tiered ACP sidecar resolution (Spec 4 §4), shared by the CLI and cockpit.
//!
//! Order: explicit `RYUZI_ACP_PATH` override → cached artifact → download.
//! The adapter is NEVER searched for on PATH (Spec 3 invariant); probing for
//! the *bun runtime* on PATH is allowed. sha256 digests are pinned in the
//! host-embedded manifest; a mismatched artifact is deleted, never executed.

use std::path::{Path, PathBuf};

use anyhow::{bail, Context};

pub const RELEASE_BASE: &str = "https://github.com/alfin-efendy/ryuzi/releases/download";
pub const ADAPTER_BIN: &str = "claude-agent-acp";

#[derive(Debug, Clone, serde::Deserialize)]
pub struct ArtifactSpec {
    pub asset: String,
    pub sha256: String,
}

#[derive(Debug, Clone, serde::Deserialize)]
pub struct SidecarManifest {
    pub version: String,
    pub min_bun: String,
    /// The GitHub release tag (`vX.Y.Z`) that hosts the artifacts named in
    /// `bundle`/`standalone`. Required — no serde default — because the
    /// manifest is host-controlled: the release pipeline overwrites this
    /// file with the real tag before building release binaries, so a
    /// missing tag would silently resolve against the wrong release.
    pub release_tag: String,
    pub bundle: ArtifactSpec,
    pub standalone: std::collections::HashMap<String, ArtifactSpec>,
}

pub trait Fetcher: Send + Sync {
    fn fetch(&self, url: &str, dest: &Path) -> anyhow::Result<()>;
}

pub struct HttpFetcher;

impl Fetcher for HttpFetcher {
    fn fetch(&self, url: &str, dest: &Path) -> anyhow::Result<()> {
        let resp = ureq::get(url)
            .call()
            .with_context(|| format!("download {url}"))?;
        let mut file = std::fs::File::create(dest)?;
        std::io::copy(&mut resp.into_reader(), &mut file)?;
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum SidecarMode {
    Override,
    Bun,
    Standalone,
}

#[derive(Debug, Clone)]
pub struct ResolvedSidecar {
    pub command: String,
    pub args: Vec<String>,
    pub mode: SidecarMode,
}

#[derive(Debug, Clone, PartialEq)]
pub enum SidecarStatus {
    Override,
    CachedBundle,
    CachedStandalone,
    NeedsDownloadBundle,
    NeedsDownloadStandalone,
}

pub struct SidecarConfig {
    pub manifest: SidecarManifest,
    pub cache_dir: PathBuf,
    pub target: String,
    pub release_tag: String,
    pub override_path: Option<PathBuf>,
    pub bun_probe: fn() -> Option<String>,
}

pub fn default_bun_probe() -> Option<String> {
    let output = std::process::Command::new("bun")
        .arg("--version")
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    Some(String::from_utf8_lossy(&output.stdout).trim().to_string())
}

/// Numeric-triple comparison; non-numeric segments compare as 0.
pub fn semver_ge(a: &str, b: &str) -> bool {
    let parse = |s: &str| -> [u64; 3] {
        let mut out = [0u64; 3];
        for (i, part) in s.split('.').take(3).enumerate() {
            out[i] = part.trim().parse().unwrap_or(0);
        }
        out
    };
    parse(a) >= parse(b)
}

pub struct SidecarManager {
    cfg: SidecarConfig,
    fetcher: Box<dyn Fetcher>,
}

impl SidecarManager {
    pub fn new(cfg: SidecarConfig, fetcher: Box<dyn Fetcher>) -> Self {
        Self { cfg, fetcher }
    }

    fn version_dir(&self) -> PathBuf {
        self.cfg
            .cache_dir
            .join("acp")
            .join(&self.cfg.manifest.version)
    }

    fn bundle_path(&self) -> PathBuf {
        self.version_dir().join("adapter.js")
    }

    fn standalone_path(&self) -> PathBuf {
        let name = if self.cfg.target.contains("windows") {
            format!("{ADAPTER_BIN}.exe")
        } else {
            ADAPTER_BIN.to_string()
        };
        self.version_dir().join(name)
    }

    fn bun_ok(&self) -> bool {
        (self.cfg.bun_probe)()
            .map(|v| semver_ge(&v, &self.cfg.manifest.min_bun))
            .unwrap_or(false)
    }

    /// Cheap report for `doctor` — never touches the network.
    pub fn status(&self) -> SidecarStatus {
        if self.cfg.override_path.is_some() {
            return SidecarStatus::Override;
        }
        if self.bundle_path().exists() && self.bun_ok() {
            return SidecarStatus::CachedBundle;
        }
        if self.standalone_path().exists() {
            return SidecarStatus::CachedStandalone;
        }
        if self.bun_ok() {
            SidecarStatus::NeedsDownloadBundle
        } else {
            SidecarStatus::NeedsDownloadStandalone
        }
    }

    /// Resolve to a spawnable command, downloading + verifying on first use.
    /// Re-evaluated per call: if bun disappeared after a bundle was cached, we
    /// fall through to the standalone path.
    pub fn resolve(&self) -> anyhow::Result<ResolvedSidecar> {
        if let Some(p) = &self.cfg.override_path {
            if !p.exists() {
                bail!("RYUZI_ACP_PATH points to a missing file: {}", p.display());
            }
            return Ok(ResolvedSidecar {
                command: p.to_string_lossy().into_owned(),
                args: vec![],
                mode: SidecarMode::Override,
            });
        }

        if self.bun_ok() {
            let bundle = self.bundle_path();
            if !bundle.exists() {
                self.download(&self.cfg.manifest.bundle.clone(), &bundle, false)?;
            }
            return Ok(ResolvedSidecar {
                command: "bun".to_string(),
                args: vec![bundle.to_string_lossy().into_owned()],
                mode: SidecarMode::Bun,
            });
        }

        let spec = self
            .cfg
            .manifest
            .standalone
            .get(&self.cfg.target)
            .with_context(|| {
                format!(
                    "no prebuilt ACP adapter for target {} — install bun >= {} or set RYUZI_ACP_PATH",
                    self.cfg.target, self.cfg.manifest.min_bun
                )
            })?
            .clone();
        let dest = self.standalone_path();
        if !dest.exists() {
            self.download(&spec, &dest, true)?;
        }
        Ok(ResolvedSidecar {
            command: dest.to_string_lossy().into_owned(),
            args: vec![],
            mode: SidecarMode::Standalone,
        })
    }

    /// Atomic download: fetch to a per-process `<dest>.partial.<pid>`, verify
    /// sha256, rename. A mismatched artifact is deleted and never lands at
    /// `dest`. The partial path is unique per process so two processes
    /// (e.g. CLI + cockpit) cold-starting concurrently never interleave
    /// writes into the same file; the rename remains the atomic commit
    /// point, so last-writer-wins is safe since both wrote identical,
    /// verified bytes. When `executable` is set, the exec bit is applied to
    /// the partial (on unix) before the rename so the file at `dest` is
    /// never observed in a non-executable state.
    fn download(&self, spec: &ArtifactSpec, dest: &Path, executable: bool) -> anyhow::Result<()> {
        std::fs::create_dir_all(dest.parent().expect("dest has parent"))?;
        // Sweep partials left by crashed processes. Our own partial has this
        // process's pid suffix and doesn't exist yet, so everything matching
        // "*.partial.*" is stale and safe to remove (best-effort).
        if let Ok(entries) = std::fs::read_dir(dest.parent().expect("dest has parent")) {
            for entry in entries.flatten() {
                if entry.file_name().to_string_lossy().contains(".partial.") {
                    let _ = std::fs::remove_file(entry.path());
                }
            }
        }
        let url = format!("{RELEASE_BASE}/{}/{}", self.cfg.release_tag, spec.asset);
        let partial = dest.with_extension(format!("partial.{}", std::process::id()));
        self.fetcher.fetch(&url, &partial)?;
        let bytes = std::fs::read(&partial)?;
        let actual = {
            use sha2::{Digest, Sha256};
            format!("{:x}", Sha256::digest(&bytes))
        };
        if !actual.eq_ignore_ascii_case(&spec.sha256) {
            let _ = std::fs::remove_file(&partial);
            bail!(
                "sha256 mismatch for {} (expected {}, got {actual}) — refusing to use it",
                spec.asset,
                spec.sha256
            );
        }
        #[cfg(unix)]
        if executable {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&partial, std::fs::Permissions::from_mode(0o755))?;
        }
        #[cfg(not(unix))]
        let _ = executable;
        if let Err(e) = std::fs::rename(&partial, dest) {
            let _ = std::fs::remove_file(&partial);
            return Err(e).with_context(|| format!("rename partial to {}", dest.display()));
        }
        Ok(())
    }
}

/// The manifest pinned into this build. Hosts (CLI + cockpit) share it; the
/// release pipeline overwrites this file (with the real tag + shas) before
/// building release binaries.
pub fn embedded_manifest() -> SidecarManifest {
    serde_json::from_str(include_str!("../sidecar.manifest.json"))
        .expect("sidecar.manifest.json is validated at build time")
}

/// The production resolver both hosts use: embedded manifest, this build's
/// target triple, the shared cache dir, RYUZI_ACP_PATH override, bun probe.
pub fn host_manager() -> SidecarManager {
    let manifest = embedded_manifest();
    let release_tag = manifest.release_tag.clone();
    SidecarManager::new(
        SidecarConfig {
            manifest,
            cache_dir: crate::paths::state_dir().join("sidecars"),
            target: env!("RYUZI_TARGET").to_string(),
            release_tag,
            override_path: std::env::var_os("RYUZI_ACP_PATH").map(std::path::PathBuf::from),
            bun_probe: default_bun_probe,
        },
        Box::new(HttpFetcher),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;
    use std::path::Path;

    struct FakeFetcher(Vec<u8>);
    impl Fetcher for FakeFetcher {
        fn fetch(&self, _url: &str, dest: &Path) -> anyhow::Result<()> {
            std::fs::write(dest, &self.0)?;
            Ok(())
        }
    }

    fn sha256_hex(bytes: &[u8]) -> String {
        use sha2::{Digest, Sha256};
        format!("{:x}", Sha256::digest(bytes))
    }

    fn manifest(bundle_sha: &str, bin_sha: &str) -> SidecarManifest {
        SidecarManifest {
            version: "0.55.0".into(),
            min_bun: "1.3.14".into(),
            release_tag: "v0.3.0".into(),
            bundle: ArtifactSpec {
                asset: "claude-agent-acp-0.55.0.js".into(),
                sha256: bundle_sha.into(),
            },
            standalone: HashMap::from([(
                "x86_64-unknown-linux-gnu".into(),
                ArtifactSpec {
                    asset: "claude-agent-acp-0.55.0-x86_64-unknown-linux-gnu".into(),
                    sha256: bin_sha.into(),
                },
            )]),
        }
    }

    fn cfg(dir: &Path, m: SidecarManifest, bun: fn() -> Option<String>) -> SidecarConfig {
        SidecarConfig {
            manifest: m,
            cache_dir: dir.to_path_buf(),
            target: "x86_64-unknown-linux-gnu".into(),
            release_tag: "v0.3.0".into(),
            override_path: None,
            bun_probe: bun,
        }
    }

    #[test]
    fn semver_ge_compares_numeric_triples() {
        assert!(semver_ge("1.3.14", "1.3.14"));
        assert!(semver_ge("1.10.0", "1.3.14"));
        assert!(!semver_ge("1.3.9", "1.3.14"));
    }

    #[test]
    fn override_path_wins_and_must_exist() {
        let dir = tempfile::tempdir().unwrap();
        let fake = dir.path().join("my-adapter");
        std::fs::write(&fake, b"x").unwrap();
        let mut c = cfg(dir.path(), manifest("00", "00"), || None);
        c.override_path = Some(fake.clone());
        let mgr = SidecarManager::new(c, Box::new(FakeFetcher(vec![])));
        let r = mgr.resolve().unwrap();
        assert_eq!(r.mode, SidecarMode::Override);
        assert_eq!(r.command, fake.to_string_lossy());
        assert!(r.args.is_empty());

        let mut c2 = cfg(dir.path(), manifest("00", "00"), || None);
        c2.override_path = Some(dir.path().join("missing"));
        assert!(SidecarManager::new(c2, Box::new(FakeFetcher(vec![])))
            .resolve()
            .is_err());
    }

    #[test]
    fn bun_present_downloads_bundle_and_spawns_via_bun() {
        let dir = tempfile::tempdir().unwrap();
        let body = b"js bundle bytes".to_vec();
        let m = manifest(&sha256_hex(&body), "00");
        let mgr = SidecarManager::new(
            cfg(dir.path(), m, || Some("1.3.14".into())),
            Box::new(FakeFetcher(body)),
        );
        assert_eq!(mgr.status(), SidecarStatus::NeedsDownloadBundle);
        let r = mgr.resolve().unwrap();
        assert_eq!(r.mode, SidecarMode::Bun);
        assert_eq!(r.command, "bun");
        let cached = dir.path().join("acp/0.55.0/adapter.js");
        assert_eq!(r.args, vec![cached.to_string_lossy().to_string()]);
        assert!(cached.exists());
        assert_eq!(mgr.status(), SidecarStatus::CachedBundle);
    }

    #[test]
    fn old_bun_falls_through_to_standalone() {
        let dir = tempfile::tempdir().unwrap();
        let body = b"native binary bytes".to_vec();
        let m = manifest("00", &sha256_hex(&body));
        let mgr = SidecarManager::new(
            cfg(dir.path(), m, || Some("1.0.0".into())),
            Box::new(FakeFetcher(body)),
        );
        assert_eq!(mgr.status(), SidecarStatus::NeedsDownloadStandalone);
        let r = mgr.resolve().unwrap();
        assert_eq!(r.mode, SidecarMode::Standalone);
        assert!(
            r.command.ends_with("claude-agent-acp") || r.command.ends_with("claude-agent-acp.exe")
        );
        assert!(r.args.is_empty());
    }

    #[test]
    fn sha_mismatch_deletes_artifact_and_errors() {
        let dir = tempfile::tempdir().unwrap();
        let m = manifest(&sha256_hex(b"expected"), "00");
        let mgr = SidecarManager::new(
            cfg(dir.path(), m, || Some("1.3.14".into())),
            Box::new(FakeFetcher(b"tampered".to_vec())),
        );
        assert!(mgr.resolve().is_err());
        assert!(!dir.path().join("acp/0.55.0/adapter.js").exists());
    }

    #[cfg(unix)]
    #[test]
    fn standalone_artifact_is_executable_at_final_path() {
        use std::os::unix::fs::PermissionsExt;

        let dir = tempfile::tempdir().unwrap();
        let body = b"native binary bytes".to_vec();
        let m = manifest("00", &sha256_hex(&body));
        let mgr = SidecarManager::new(
            cfg(dir.path(), m, || Some("1.0.0".into())),
            Box::new(FakeFetcher(body)),
        );
        let r = mgr.resolve().unwrap();
        assert_eq!(r.mode, SidecarMode::Standalone);
        let final_path = PathBuf::from(&r.command);
        let mode = std::fs::metadata(&final_path).unwrap().permissions().mode();
        assert_ne!(mode & 0o111, 0, "expected exec bit set on {final_path:?}");
    }

    #[test]
    fn stale_partials_from_other_processes_are_swept_before_download() {
        let dir = tempfile::tempdir().unwrap();
        let body = b"js bundle bytes".to_vec();
        let m = manifest(&sha256_hex(&body), "00");
        // A crashed process (fake pid 99999999) left a stale partial behind.
        let vdir = dir.path().join("acp/0.55.0");
        std::fs::create_dir_all(&vdir).unwrap();
        let stale = vdir.join("adapter.partial.99999999");
        std::fs::write(&stale, b"junk").unwrap();

        let mgr = SidecarManager::new(
            cfg(dir.path(), m, || Some("1.3.14".into())),
            Box::new(FakeFetcher(body)),
        );
        mgr.resolve().unwrap();

        assert!(!stale.exists(), "stale partial must be swept");
        assert!(vdir.join("adapter.js").exists());
    }

    #[test]
    fn embedded_manifest_parses_and_names_a_release_tag() {
        let m = embedded_manifest();
        assert!(
            m.release_tag.starts_with('v'),
            "release_tag must be a vX.Y.Z tag: {}",
            m.release_tag
        );
        assert!(!m.bundle.asset.is_empty());
        assert!(!m.min_bun.is_empty());
    }

    #[test]
    fn unknown_target_errors_with_guidance() {
        let dir = tempfile::tempdir().unwrap();
        let mut c = cfg(dir.path(), manifest("00", "00"), || None);
        c.target = "sparc64-unknown-none".into();
        let err = SidecarManager::new(c, Box::new(FakeFetcher(vec![])))
            .resolve()
            .unwrap_err();
        assert!(err.to_string().contains("RYUZI_ACP_PATH"));
    }
}
