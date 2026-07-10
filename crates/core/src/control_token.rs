//! Bearer token for the localhost control API. Persisted across daemon
//! restarts on the same control directory: as long as the control port
//! stays fixed (the default case), a restart (crash recovery, canary
//! self-update promote) keeps handing out the SAME token, so long-lived
//! clients that capture it once — Cockpit's `Arc<EngineClient>` — stay
//! authenticated without a manual restart. The token is only rotated when
//! no valid token is on disk yet, or when an existing token file is found
//! with loose (non-0600) permissions — treated as a possible exposure, so
//! it is not trusted for reuse. Same-user access is the trust boundary,
//! enforced by 0600 perms.

use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};

pub fn token_path(dir: &Path) -> PathBuf {
    dir.join("control.token")
}

/// Return the control token for `dir`, reusing an existing valid one when
/// possible instead of always minting a new one.
///
/// - If a token file already exists, is non-empty, and (on unix) already has
///   0600 perms, it is returned as-is with no rewrite — this is what lets a
///   same-port daemon restart keep a previously-issued client valid.
/// - If a token file exists but (on unix) has loose permissions, it may have
///   been readable by other local users/processes, so it is NOT reused: a
///   fresh 64-hex-char token is generated and written 0600 from birth,
///   exactly as in the "no token yet" case below. (Tightening the loose
///   file's perms and keeping its content was considered, but a token that
///   was ever world/group-readable should be treated as compromised rather
///   than re-trusted.)
/// - If no token file exists (or it's empty), a fresh 64-hex-char token is
///   generated and written 0600.
pub fn write_token(dir: &Path) -> anyhow::Result<String> {
    let path = token_path(dir);

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if let Some(existing) = read_token(dir) {
            let is_0600 = std::fs::metadata(&path)
                .map(|m| m.permissions().mode() & 0o777 == 0o600)
                .unwrap_or(false);
            if is_0600 {
                return Ok(existing);
            }
            // Loose perms: fall through and regenerate below rather than
            // trusting the possibly-exposed existing secret.
        }
    }
    #[cfg(not(unix))]
    {
        if let Some(existing) = read_token(dir) {
            return Ok(existing);
        }
    }

    let token = format!(
        "{}{}",
        uuid::Uuid::new_v4().simple(),
        uuid::Uuid::new_v4().simple()
    );
    #[cfg(unix)]
    {
        use std::io::Write;
        use std::os::unix::fs::OpenOptionsExt;
        use std::os::unix::fs::PermissionsExt;
        let mut f = std::fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .mode(0o600) // applies at creation — file is born 0600
            .open(&path)?;
        // A pre-existing file (older daemon run, or one found with loose
        // perms above) keeps its old mode across truncate; tighten it before
        // any bytes land.
        f.set_permissions(std::fs::Permissions::from_mode(0o600))?;
        f.write_all(token.as_bytes())?;
    }
    #[cfg(not(unix))]
    std::fs::write(&path, &token)?;
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

    #[cfg(unix)]
    #[test]
    fn write_token_regenerates_pre_existing_loose_permissions() {
        // A loose-perm token file is treated as possibly exposed, not
        // reused: write_token must replace its content with a fresh 0600
        // token rather than tightening perms and keeping the stale secret.
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let path = token_path(dir.path());
        std::fs::write(&path, "stale-token-from-older-run").unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();

        let token = write_token(dir.path()).unwrap();

        let mode = std::fs::metadata(&path).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o600);
        assert_ne!(
            token, "stale-token-from-older-run",
            "loose-perm token must be regenerated, not reused"
        );
        assert_eq!(read_token(dir.path()), Some(token));
    }

    #[test]
    fn write_token_called_twice_reuses_same_token() {
        // Simulates a same-port daemon restart: the token must not rotate,
        // or every client holding a fixed Arc<EngineClient> (e.g. Cockpit)
        // would start getting 401s after the restart.
        let dir = tempfile::tempdir().unwrap();
        let first = write_token(dir.path()).unwrap();
        let second = write_token(dir.path()).unwrap();
        assert_eq!(first, second);
    }

    #[cfg(unix)]
    #[test]
    fn write_token_preserves_pre_existing_0600_token() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let path = token_path(dir.path());
        let existing = "already-issued-token-from-a-prior-daemon-run";
        std::fs::write(&path, existing).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();

        let token = write_token(dir.path()).unwrap();

        assert_eq!(token, existing);
        assert_eq!(read_token(dir.path()), Some(existing.to_string()));
    }

    #[test]
    fn read_token_none_when_missing_or_empty() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(read_token(dir.path()), None);
        std::fs::write(token_path(dir.path()), "  \n").unwrap();
        assert_eq!(read_token(dir.path()), None);
    }
}
