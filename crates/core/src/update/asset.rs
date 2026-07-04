//! Port of `packages/core/src/update/asset.ts` with the NEW (design §10)
//! asset-naming scheme: `ryuzi-{version}-{target-triple}.tar.gz`. The break
//! from the TS `ryuzi_{v}_{goos}_{goarch}` scheme is deliberate — in-field TS
//! self-updaters match assets by name, find nothing, and stay silent.
use sha2::{Digest, Sha256};

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Platform {
    /// `std::env::consts::OS` value: "linux" | "macos".
    pub os: &'static str,
    /// `std::env::consts::ARCH` value: "x86_64" | "aarch64".
    pub arch: &'static str,
    pub musl: bool,
}

pub fn target_triple(p: Platform) -> Option<&'static str> {
    match (p.os, p.arch, p.musl) {
        ("linux", "x86_64", false) => Some("x86_64-unknown-linux-gnu"),
        ("linux", "x86_64", true) => Some("x86_64-unknown-linux-musl"),
        ("linux", "aarch64", false) => Some("aarch64-unknown-linux-gnu"),
        ("linux", "aarch64", true) => Some("aarch64-unknown-linux-musl"),
        ("macos", "x86_64", _) => Some("x86_64-apple-darwin"),
        ("macos", "aarch64", _) => Some("aarch64-apple-darwin"),
        _ => None,
    }
}

pub fn asset_name(version: &str, p: Platform) -> Option<String> {
    Some(format!("ryuzi-{version}-{}.tar.gz", target_triple(p)?))
}

pub fn asset_url(repo: &str, tag: &str, name: &str) -> String {
    format!("https://github.com/{repo}/releases/download/{tag}/{name}")
}

pub fn checksums_url(repo: &str, tag: &str) -> String {
    format!("https://github.com/{repo}/releases/download/{tag}/checksums.txt")
}

pub fn sha256_hex(bytes: &[u8]) -> String {
    let mut h = Sha256::new();
    h.update(bytes);
    format!("{:x}", h.finalize())
}

/// checksums.txt lines are `"<sha256>  <filename>"`; verify the entry for `name`.
pub fn verify_checksum(bytes: &[u8], name: &str, checksums_text: &str) -> bool {
    let want = sha256_hex(bytes);
    for line in checksums_text.lines() {
        let trimmed = line.trim();
        let Some(sp) = trimmed.find(' ') else {
            continue;
        };
        let (hash, file) = (&trimmed[..sp], trimmed[sp..].trim());
        if file == name {
            return hash.eq_ignore_ascii_case(&want);
        }
    }
    false
}

/// Compile-time-informed platform detect. Deliberate delta from TS (which
/// sniffed `/etc/alpine-release`): `cfg!(target_env = "musl")` — the binary
/// knows its own linkage, and a self-update must fetch the same flavor.
pub fn detect_platform() -> Option<Platform> {
    let p = Platform {
        os: match std::env::consts::OS {
            "linux" => "linux",
            "macos" => "macos",
            _ => return None,
        },
        arch: match std::env::consts::ARCH {
            "x86_64" => "x86_64",
            "aarch64" => "aarch64",
            _ => return None,
        },
        musl: cfg!(target_env = "musl"),
    };
    target_triple(p).map(|_| p)
}

#[cfg(test)]
mod tests {
    use super::*;

    const LINUX_GNU: Platform = Platform {
        os: "linux",
        arch: "x86_64",
        musl: false,
    };

    #[test]
    fn asset_name_uses_the_new_triple_scheme() {
        assert_eq!(
            asset_name("0.3.0", LINUX_GNU).as_deref(),
            Some("ryuzi-0.3.0-x86_64-unknown-linux-gnu.tar.gz")
        );
        assert_eq!(
            asset_name(
                "0.3.0",
                Platform {
                    os: "linux",
                    arch: "aarch64",
                    musl: true
                }
            )
            .as_deref(),
            Some("ryuzi-0.3.0-aarch64-unknown-linux-musl.tar.gz")
        );
        assert_eq!(
            asset_name(
                "0.3.0",
                Platform {
                    os: "macos",
                    arch: "aarch64",
                    musl: false
                }
            )
            .as_deref(),
            Some("ryuzi-0.3.0-aarch64-apple-darwin.tar.gz")
        );
        // the old TS goos/goarch scheme must be impossible to produce
        assert!(asset_name("0.3.0", LINUX_GNU)
            .unwrap()
            .contains("x86_64-unknown-linux-gnu"));
        assert!(asset_name(
            "0.3.0",
            Platform {
                os: "windows",
                arch: "x86_64",
                musl: false
            }
        )
        .is_none());
    }

    #[test]
    fn urls_point_at_github_release_downloads() {
        assert_eq!(
            asset_url("o/r", "v0.3.0", "a.tar.gz"),
            "https://github.com/o/r/releases/download/v0.3.0/a.tar.gz"
        );
        assert_eq!(
            checksums_url("o/r", "v0.3.0"),
            "https://github.com/o/r/releases/download/v0.3.0/checksums.txt"
        );
    }

    #[test]
    fn verify_checksum_matches_the_named_entry_case_insensitively() {
        let bytes = b"tarball";
        let sum = sha256_hex(bytes);
        let name = "ryuzi-0.3.0-x86_64-unknown-linux-gnu.tar.gz";
        let ok = format!("{sum}  {name}\n");
        assert!(verify_checksum(bytes, name, &ok));
        assert!(verify_checksum(
            bytes,
            name,
            &ok.to_uppercase().replace(&name.to_uppercase(), name)
        ));
        assert!(!verify_checksum(
            bytes,
            name,
            &format!("deadbeef  {name}\n")
        ));
        assert!(!verify_checksum(bytes, name, "")); // no entry → false
        assert!(!verify_checksum(
            bytes,
            name,
            &format!("{sum}  other.tar.gz\n")
        ));
        // tolerates blank lines and single-space separators
        assert!(verify_checksum(bytes, name, &format!("\n{sum} {name}\n\n")));
    }

    #[test]
    fn detect_platform_is_some_on_supported_hosts() {
        // On CI (linux/mac x86_64/aarch64) this must resolve to a triple.
        let p = detect_platform().expect("supported host");
        assert!(target_triple(p).is_some());
    }
}
