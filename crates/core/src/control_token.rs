//! Bearer token for the localhost control API. Regenerated at every daemon
//! start; clients (Cockpit, CLI attach) read it from disk — same-user access
//! is the trust boundary, enforced by 0600 perms.

use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};

pub fn token_path(dir: &Path) -> PathBuf {
    dir.join("control.token")
}

/// Generate a fresh 64-hex-char token, write it 0600, return it.
pub fn write_token(dir: &Path) -> anyhow::Result<String> {
    let token = format!(
        "{}{}",
        uuid::Uuid::new_v4().simple(),
        uuid::Uuid::new_v4().simple()
    );
    let path = token_path(dir);
    std::fs::write(&path, &token)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600))?;
    }
    Ok(token)
}

pub fn read_token(dir: &Path) -> Option<String> {
    let t = std::fs::read_to_string(token_path(dir)).ok()?;
    let t = t.trim().to_string();
    if t.is_empty() {
        None
    } else {
        Some(t)
    }
}

/// Timing-safe-enough comparison: compare SHA-256 digests with `==` so the
/// byte-by-byte early exit never operates on the secret itself.
pub fn verify(presented: &str, expected: &str) -> bool {
    Sha256::digest(presented.as_bytes()) == Sha256::digest(expected.as_bytes())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip_and_verify() {
        let dir = tempfile::tempdir().unwrap();
        let token = write_token(dir.path()).unwrap();
        assert_eq!(token.len(), 64);
        assert_eq!(read_token(dir.path()), Some(token.clone()));
        assert!(verify(&token, &token));
        assert!(!verify("wrong", &token));
    }

    #[cfg(unix)]
    #[test]
    fn token_file_is_0600() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        write_token(dir.path()).unwrap();
        let mode = std::fs::metadata(token_path(dir.path()))
            .unwrap()
            .permissions()
            .mode();
        assert_eq!(mode & 0o777, 0o600);
    }

    #[test]
    fn read_token_none_when_missing_or_empty() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(read_token(dir.path()), None);
        std::fs::write(token_path(dir.path()), "  \n").unwrap();
        assert_eq!(read_token(dir.path()), None);
    }
}
