//! Import Kiro OAuth tokens from an already-installed, logged-in Kiro IDE:
//! read its AWS SSO token cache + client registration + profile.json so a
//! connection can be created without running the device-code flow again.
//! Ported from 9router (MIT, (c) 2024-2026 decolua and contributors) —
//! src/app/api/oauth/kiro/auto-import/route.js + import/route.js.
use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{anyhow, Result};

/// Prefix that a Kiro refresh token always starts with — used both to pick
/// `kiro-auth-token.json` when it's valid and to recognize the right file
/// when scanning the rest of the SSO cache directory.
const KIRO_REFRESH_TOKEN_PREFIX: &str = "aorAAAAAG";

/// Tokens + client credentials read out of an installed Kiro IDE's local
/// cache, ready to hand to [`super::super::connections`] as a new
/// connection's `ConnectionData`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImportedKiro {
    pub refresh_token: String,
    pub region: Option<String>,
    /// `"idc"` when a linked client registration (clientId + clientSecret)
    /// was found, else `"imported"`.
    pub auth_method: String,
    pub client_id: Option<String>,
    pub client_secret: Option<String>,
    /// CodeWhisperer profile ARN, always normalized to the `us-east-1`
    /// region segment regardless of what region the IDC token itself uses.
    pub profile_arn: Option<String>,
}

/// Read tokens from the real, OS-specific Kiro IDE cache locations: the AWS
/// SSO token cache under the user's home directory, and (on Windows)
/// Kiro's `profile.json` under `%APPDATA%` — or under `~/.config` elsewhere.
pub fn read_kiro_ide_cache() -> Result<ImportedKiro> {
    let home = dirs::home_dir().ok_or_else(not_found_err)?;
    let sso_dir = home.join(".aws").join("sso").join("cache");

    let profile_json = if cfg!(windows) {
        dirs::config_dir().map(|d| d.join("Kiro/User/globalStorage/kiro.kiroagent/profile.json"))
    } else {
        dirs::home_dir()
            .map(|d| d.join(".config/Kiro/User/globalStorage/kiro.kiroagent/profile.json"))
    };

    read_kiro_ide_cache_from(&sso_dir, profile_json.as_deref())
}

/// Same as [`read_kiro_ide_cache`] but against explicit paths — the test
/// seam so specs can point this at a fixture directory instead of the real
/// OS locations.
pub fn read_kiro_ide_cache_from(
    sso_dir: &Path,
    profile_json: Option<&Path>,
) -> Result<ImportedKiro> {
    // The raw `authMethod` field from the token file is superseded by the
    // resolved `auth_method` below (derived from client-creds presence).
    let (refresh_token, region, _raw_auth_method, client_id_hash) = read_token(sso_dir)?;

    let (client_id, client_secret) = match client_id_hash.as_deref() {
        Some(hash) => read_client_registration(sso_dir, hash),
        None => (None, None),
    };
    let auth_method = if client_id.is_some() && client_secret.is_some() {
        "idc"
    } else {
        "imported"
    }
    .to_string();

    let profile_arn = profile_json
        .and_then(read_profile_arn)
        .map(|arn| normalize_profile_region(&arn));

    Ok(ImportedKiro {
        refresh_token,
        region,
        auth_method,
        client_id,
        client_secret,
        profile_arn,
    })
}

fn not_found_err() -> anyhow::Error {
    anyhow!("Kiro IDE not found or not logged in — sign into Kiro IDE, then import again.")
}

/// `(refresh_token, region, auth_method, client_id_hash)` read from a
/// candidate token JSON file, before any resolution against the client
/// registration / profile.json files.
type RawTokenFields = (String, Option<String>, Option<String>, Option<String>);

/// Find and read the token file: try `kiro-auth-token.json` first, falling
/// back to scanning every `*.json` file in `sso_dir` for one whose
/// `refreshToken` matches the Kiro prefix.
fn read_token(sso_dir: &Path) -> Result<RawTokenFields> {
    let entries = fs::read_dir(sso_dir).map_err(|_| not_found_err())?;

    let primary = sso_dir.join("kiro-auth-token.json");
    if let Some(found) = read_token_file(&primary) {
        return Ok(found);
    }

    let mut json_files: Vec<PathBuf> = entries
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| p.extension().and_then(|e| e.to_str()) == Some("json"))
        .collect();
    json_files.sort();

    for path in json_files {
        if let Some(found) = read_token_file(&path) {
            return Ok(found);
        }
    }

    Err(not_found_err())
}

/// Read and parse a single candidate token file, returning `Some(..)` only
/// if it has a `refreshToken` starting with [`KIRO_REFRESH_TOKEN_PREFIX`].
fn read_token_file(path: &Path) -> Option<RawTokenFields> {
    let content = fs::read_to_string(path).ok()?;
    let json: serde_json::Value = serde_json::from_str(&content).ok()?;
    let refresh_token = json.get("refreshToken").and_then(|v| v.as_str())?;
    if !refresh_token.starts_with(KIRO_REFRESH_TOKEN_PREFIX) {
        return None;
    }
    let region = json
        .get("region")
        .and_then(|v| v.as_str())
        .filter(|s| !s.trim().is_empty())
        .map(String::from);
    let auth_method = json
        .get("authMethod")
        .and_then(|v| v.as_str())
        .filter(|s| !s.trim().is_empty())
        .map(String::from);
    let client_id_hash = json
        .get("clientIdHash")
        .and_then(|v| v.as_str())
        .filter(|s| !s.trim().is_empty())
        .map(String::from);
    Some((
        refresh_token.to_string(),
        region,
        auth_method,
        client_id_hash,
    ))
}

/// Read the sibling `{client_id_hash}.json` client-registration file. The
/// file being absent (or unparseable, or missing either field) is tolerated
/// — the caller falls back to "no client creds" (`auth_method = "imported"`).
fn read_client_registration(
    sso_dir: &Path,
    client_id_hash: &str,
) -> (Option<String>, Option<String>) {
    let path = sso_dir.join(format!("{client_id_hash}.json"));
    let Ok(content) = fs::read_to_string(&path) else {
        return (None, None);
    };
    let Ok(json) = serde_json::from_str::<serde_json::Value>(&content) else {
        return (None, None);
    };
    let client_id = json
        .get("clientId")
        .and_then(|v| v.as_str())
        .filter(|s| !s.trim().is_empty())
        .map(String::from);
    let client_secret = json
        .get("clientSecret")
        .and_then(|v| v.as_str())
        .filter(|s| !s.trim().is_empty())
        .map(String::from);
    (client_id, client_secret)
}

/// Read `arn` out of Kiro's `profile.json`, if present and parseable.
/// Absence is tolerated — the caller yields `profile_arn: None`.
fn read_profile_arn(profile_json: &Path) -> Option<String> {
    let content = fs::read_to_string(profile_json).ok()?;
    let json: serde_json::Value = serde_json::from_str(&content).ok()?;
    json.get("arn")
        .and_then(|v| v.as_str())
        .filter(|s| !s.trim().is_empty())
        .map(String::from)
}

/// Replace the `arn:aws:codewhisperer:<region>:` prefix with `us-east-1` —
/// the runtime gateway requires that region in the ARN regardless of what
/// region the IDC token itself uses.
/// Ported from 9router (MIT, (c) 2024-2026 decolua and contributors).
fn normalize_profile_region(arn: &str) -> String {
    if let Some(rest) = arn.strip_prefix("arn:aws:codewhisperer:") {
        if let Some(idx) = rest.find(':') {
            return format!("arn:aws:codewhisperer:us-east-1:{}", &rest[idx + 1..]);
        }
    }
    arn.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    /// Build a fresh scratch directory under the OS temp dir for one test's
    /// fixture files, named after `label` plus a random suffix so parallel
    /// tests never collide.
    fn scratch_dir(label: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "ryuzi-import-kiro-{label}-{}",
            uuid::Uuid::new_v4()
        ));
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn reads_idc_token_with_client_creds_and_normalizes_profile_region() {
        let base = scratch_dir("idc");
        let sso_dir = base.join("sso-cache");
        fs::create_dir_all(&sso_dir).unwrap();
        fs::write(
            sso_dir.join("kiro-auth-token.json"),
            r#"{"refreshToken":"aorAAAAAGxxxx","region":"us-east-1","authMethod":"builder-id","clientIdHash":"abc"}"#,
        )
        .unwrap();
        fs::write(
            sso_dir.join("abc.json"),
            r#"{"clientId":"c","clientSecret":"s"}"#,
        )
        .unwrap();
        let profile_json = base.join("profile.json");
        fs::write(
            &profile_json,
            r#"{"arn":"arn:aws:codewhisperer:eu-west-1:1:profile/X"}"#,
        )
        .unwrap();

        let imported = read_kiro_ide_cache_from(&sso_dir, Some(&profile_json)).unwrap();

        assert_eq!(imported.refresh_token, "aorAAAAAGxxxx");
        assert_eq!(imported.region.as_deref(), Some("us-east-1"));
        assert_eq!(imported.client_id.as_deref(), Some("c"));
        assert_eq!(imported.client_secret.as_deref(), Some("s"));
        assert_eq!(imported.auth_method, "idc");
        assert_eq!(
            imported.profile_arn.as_deref(),
            Some("arn:aws:codewhisperer:us-east-1:1:profile/X")
        );

        fs::remove_dir_all(&base).ok();
    }

    #[test]
    fn no_client_id_hash_yields_imported_auth_method_and_no_creds() {
        let base = scratch_dir("imported");
        let sso_dir = base.join("sso-cache");
        fs::create_dir_all(&sso_dir).unwrap();
        fs::write(
            sso_dir.join("kiro-auth-token.json"),
            r#"{"refreshToken":"aorAAAAAGyyyy","region":"us-east-1"}"#,
        )
        .unwrap();

        let imported = read_kiro_ide_cache_from(&sso_dir, None).unwrap();

        assert_eq!(imported.refresh_token, "aorAAAAAGyyyy");
        assert_eq!(imported.auth_method, "imported");
        assert_eq!(imported.client_id, None);
        assert_eq!(imported.client_secret, None);
        assert_eq!(imported.profile_arn, None);

        fs::remove_dir_all(&base).ok();
    }

    #[test]
    fn missing_sso_dir_yields_typed_not_found_error() {
        let missing = std::env::temp_dir().join(format!(
            "ryuzi-import-kiro-missing-{}",
            uuid::Uuid::new_v4()
        ));
        let err = read_kiro_ide_cache_from(&missing, None).unwrap_err();
        assert_eq!(
            err.to_string(),
            "Kiro IDE not found or not logged in — sign into Kiro IDE, then import again."
        );
    }

    #[test]
    fn falls_back_to_scanning_json_files_when_primary_missing_or_invalid() {
        let base = scratch_dir("scan");
        let sso_dir = base.join("sso-cache");
        fs::create_dir_all(&sso_dir).unwrap();
        // No kiro-auth-token.json — but some other cache file has the token.
        fs::write(sso_dir.join("not-a-token.json"), r#"{"foo":"bar"}"#).unwrap();
        fs::write(
            sso_dir.join("some-other-cache-entry.json"),
            r#"{"refreshToken":"aorAAAAAGzzzz","region":"eu-west-1"}"#,
        )
        .unwrap();

        let imported = read_kiro_ide_cache_from(&sso_dir, None).unwrap();
        assert_eq!(imported.refresh_token, "aorAAAAAGzzzz");
        assert_eq!(imported.region.as_deref(), Some("eu-west-1"));
        assert_eq!(imported.auth_method, "imported");

        fs::remove_dir_all(&base).ok();
    }

    #[test]
    fn missing_client_registration_file_is_tolerated() {
        let base = scratch_dir("missing-client-reg");
        let sso_dir = base.join("sso-cache");
        fs::create_dir_all(&sso_dir).unwrap();
        fs::write(
            sso_dir.join("kiro-auth-token.json"),
            r#"{"refreshToken":"aorAAAAAGwwww","clientIdHash":"missing-hash"}"#,
        )
        .unwrap();
        // Deliberately do NOT write missing-hash.json.

        let imported = read_kiro_ide_cache_from(&sso_dir, None).unwrap();
        assert_eq!(imported.client_id, None);
        assert_eq!(imported.client_secret, None);
        assert_eq!(imported.auth_method, "imported");

        fs::remove_dir_all(&base).ok();
    }

    #[test]
    fn missing_profile_json_yields_none_profile_arn() {
        let base = scratch_dir("no-profile");
        let sso_dir = base.join("sso-cache");
        fs::create_dir_all(&sso_dir).unwrap();
        fs::write(
            sso_dir.join("kiro-auth-token.json"),
            r#"{"refreshToken":"aorAAAAAGvvvv"}"#,
        )
        .unwrap();
        let missing_profile = base.join("does-not-exist.json");

        let imported = read_kiro_ide_cache_from(&sso_dir, Some(&missing_profile)).unwrap();
        assert_eq!(imported.profile_arn, None);

        fs::remove_dir_all(&base).ok();
    }

    #[test]
    fn normalize_profile_region_replaces_region_segment_only() {
        assert_eq!(
            normalize_profile_region("arn:aws:codewhisperer:eu-west-1:1:profile/X"),
            "arn:aws:codewhisperer:us-east-1:1:profile/X"
        );
        // Already us-east-1 -> unchanged (idempotent).
        assert_eq!(
            normalize_profile_region("arn:aws:codewhisperer:us-east-1:1:profile/X"),
            "arn:aws:codewhisperer:us-east-1:1:profile/X"
        );
        // Not a codewhisperer ARN at all -> returned verbatim.
        assert_eq!(normalize_profile_region("not-an-arn"), "not-an-arn");
    }
}
