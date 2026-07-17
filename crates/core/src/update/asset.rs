//! Release-asset naming, URLs, and checksum verification. The asset-naming
//! scheme is `ryuzi-{version}-{platform-tag}.tar.gz`, where the platform tag
//! is the Rust target triple minus its `-unknown` vendor segment
//! (`x86_64-linux-gnu`, `aarch64-apple-darwin`). Two deliberate breaks live
//! in this history: legacy `ryuzi_{v}_{goos}_{goarch}` TS self-updaters, and
//! `-unknown-` triple names in ryuzi <= 0.6.0 — both match assets by the old
//! name, find nothing, and stay silent instead of installing a mismatched
//! binary (reinstall via curl|sh or npm picks up the new scheme).
use sha2::{Digest, Sha256};

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Platform {
    /// `std::env::consts::OS` value: "linux" | "macos".
    pub os: &'static str,
    /// `std::env::consts::ARCH` value: "x86_64" | "aarch64".
    pub arch: &'static str,
    pub musl: bool,
}

pub fn platform_tag(p: Platform) -> Option<&'static str> {
    match (p.os, p.arch, p.musl) {
        ("linux", "x86_64", false) => Some("x86_64-linux-gnu"),
        ("linux", "x86_64", true) => Some("x86_64-linux-musl"),
        ("linux", "aarch64", false) => Some("aarch64-linux-gnu"),
        ("linux", "aarch64", true) => Some("aarch64-linux-musl"),
        ("macos", "x86_64", _) => Some("x86_64-apple-darwin"),
        ("macos", "aarch64", _) => Some("aarch64-apple-darwin"),
        _ => None,
    }
}

pub fn asset_name(version: &str, p: Platform) -> Option<String> {
    Some(format!("ryuzi-{version}-{}.tar.gz", platform_tag(p)?))
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

/// Compile-time-informed platform detect: musl-ness comes from
/// `cfg!(target_env = "musl")`, not runtime sniffing (e.g. checking for
/// `/etc/alpine-release`) — the binary knows its own linkage, and a
/// self-update must fetch the same flavor.
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
    platform_tag(p).map(|_| p)
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
    fn asset_name_uses_the_platform_tag_scheme() {
        assert_eq!(
            asset_name("0.7.0", LINUX_GNU).as_deref(),
            Some("ryuzi-0.7.0-x86_64-linux-gnu.tar.gz")
        );
        assert_eq!(
            asset_name(
                "0.7.0",
                Platform {
                    os: "linux",
                    arch: "aarch64",
                    musl: true
                }
            )
            .as_deref(),
            Some("ryuzi-0.7.0-aarch64-linux-musl.tar.gz")
        );
        assert_eq!(
            asset_name(
                "0.7.0",
                Platform {
                    os: "macos",
                    arch: "aarch64",
                    musl: false
                }
            )
            .as_deref(),
            Some("ryuzi-0.7.0-aarch64-apple-darwin.tar.gz")
        );
        // both retired schemes must be impossible to produce: the legacy
        // goos/goarch names and the pre-0.7 `-unknown-` triple names
        let linux_name = asset_name("0.7.0", LINUX_GNU).unwrap();
        assert!(linux_name.contains("x86_64-linux-gnu"));
        assert!(!linux_name.contains("-unknown-"));
        assert!(!linux_name.starts_with("ryuzi_"));
        assert!(asset_name(
            "0.7.0",
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
        let name = "ryuzi-0.7.0-x86_64-linux-gnu.tar.gz";
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
    fn detect_platform_matches_the_supported_host_matrix() {
        let detected = detect_platform();
        let supported = matches!(std::env::consts::OS, "linux" | "macos")
            && matches!(std::env::consts::ARCH, "x86_64" | "aarch64");

        assert_eq!(detected.is_some(), supported);
        assert!(detected.is_none_or(|platform| platform_tag(platform).is_some()));
    }
}
