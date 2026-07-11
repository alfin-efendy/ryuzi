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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn git_version_prefix_is_stripped() {
        assert_eq!(strip_git_prefix("git version 2.45.0"), "2.45.0");
        assert_eq!(strip_git_prefix("Git Version 2.45.0"), "2.45.0");
    }
}
