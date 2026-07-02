pub struct Detected {
    pub found: bool,
    pub version: Option<String>,
}

fn run_version_cmd(cmd: &str) -> Option<String> {
    let output = std::process::Command::new(cmd)
        .arg("--version")
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    Some(String::from_utf8_lossy(&output.stdout).trim().to_string())
}

pub(crate) fn strip_git_prefix(s: &str) -> String {
    let lower = s.to_ascii_lowercase();
    match lower.strip_prefix("git version") {
        Some(_) => s["git version".len()..].trim().to_string(),
        None => s.trim().to_string(),
    }
}

pub(crate) fn first_semver(s: &str) -> Option<String> {
    let bytes = s.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i].is_ascii_digit() {
            let start = i;
            let mut j = i;
            let mut dots = 0;
            while j < bytes.len() && (bytes[j].is_ascii_digit() || bytes[j] == b'.') {
                if bytes[j] == b'.' {
                    dots += 1;
                }
                j += 1;
            }
            let candidate = s[start..j].trim_end_matches('.');
            if dots >= 2 && candidate.matches('.').count() >= 2 {
                return Some(candidate.to_string());
            }
            i = j + 1;
        } else {
            i += 1;
        }
    }
    None
}

/// `git --version` → "git version 2.45.0" → "2.45.0". Spawn failure = not found.
pub fn detect_git() -> Detected {
    match run_version_cmd("git") {
        Some(out) => Detected {
            found: true,
            version: Some(strip_git_prefix(&out)),
        },
        None => Detected {
            found: false,
            version: None,
        },
    }
}

/// `claude --version` → first semver in stdout, falling back to trimmed stdout.
pub fn detect_claude() -> Detected {
    match run_version_cmd("claude") {
        Some(out) => {
            let version = first_semver(&out).unwrap_or(out);
            Detected {
                found: true,
                version: Some(version),
            }
        }
        None => Detected {
            found: false,
            version: None,
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn first_semver_extracts_from_noise() {
        assert_eq!(
            first_semver("2.1.89 (Claude Code)").as_deref(),
            Some("2.1.89")
        );
        assert_eq!(first_semver("no version here"), None);
        assert_eq!(first_semver("v1.2"), None); // needs three components
    }

    #[test]
    fn git_version_prefix_is_stripped() {
        assert_eq!(strip_git_prefix("git version 2.45.0"), "2.45.0");
        assert_eq!(strip_git_prefix("Git Version 2.45.0"), "2.45.0");
    }
}
