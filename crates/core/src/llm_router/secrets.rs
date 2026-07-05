//! Value-level encryption of stored secrets. Secrets are tagged
//! `enc:v1:<base64url(nonce‖ciphertext)>`; anything without the `enc:` prefix
//! is treated as legacy plaintext and passed through unchanged, so no schema
//! migration is needed — legacy rows upgrade lazily (and via a startup sweep).
use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine as _};
use chacha20poly1305::{
    aead::{Aead, Generate, Key, KeyInit},
    XChaCha20Poly1305, XNonce,
};
use keyring::{Entry, Error as KeyringError};
use serde::{Deserialize, Serialize};
use specta::Type;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

const ENC_PREFIX: &str = "enc:v1:";
const KEYCHAIN_SERVICE: &str = "ryuzi";
const KEYCHAIN_ACCOUNT: &str = "secret-cipher-v1";

pub struct SecretCipher {
    cipher: XChaCha20Poly1305,
}

impl SecretCipher {
    pub fn from_key(key: [u8; 32]) -> Self {
        // new_from_slice only fails on a wrong length; 32 is always valid here.
        let cipher = XChaCha20Poly1305::new_from_slice(&key).expect("32-byte key");
        Self { cipher }
    }

    pub fn encrypt(&self, plain: &str) -> String {
        let nonce = XNonce::generate();
        let ct = self
            .cipher
            .encrypt(&nonce, plain.as_bytes())
            .expect("XChaCha20Poly1305 encryption is infallible for in-memory data");
        let mut blob = Vec::with_capacity(24 + ct.len());
        blob.extend_from_slice(nonce.as_slice());
        blob.extend_from_slice(&ct);
        format!("{ENC_PREFIX}{}", URL_SAFE_NO_PAD.encode(&blob))
    }

    pub fn decrypt(&self, s: &str) -> anyhow::Result<String> {
        if !s.starts_with("enc:") {
            return Ok(s.to_string()); // legacy plaintext
        }
        let b64 = s.strip_prefix(ENC_PREFIX).ok_or_else(|| {
            anyhow::anyhow!("unknown secret encoding: {}", s.get(..12).unwrap_or(s))
        })?;
        let blob = URL_SAFE_NO_PAD.decode(b64)?;
        if blob.len() < 24 {
            anyhow::bail!("secret blob too short");
        }
        let (nonce_bytes, ct) = blob.split_at(24);
        let nonce = XNonce::try_from(nonce_bytes).expect("nonce_bytes is exactly 24 bytes");
        let plain = self
            .cipher
            .decrypt(&nonce, ct)
            .map_err(|_| anyhow::anyhow!("secret decryption failed (tamper/wrong key)"))?;
        Ok(String::from_utf8(plain)?)
    }
}

/// Where the process's master key actually came from. Surfaced to the UI
/// (via a later task's DTO mapping) so users know whether their secrets are
/// protected by the OS keychain or a weaker fallback.
#[derive(Clone, Copy, PartialEq, Debug, Serialize, Deserialize, Type)]
#[serde(rename_all = "camelCase")]
pub enum KeychainStatus {
    /// Master key is stored in (and was read from, or freshly provisioned
    /// into) the OS keychain.
    Ok,
    /// The OS keychain is unavailable (headless/locked/no D-Bus session), so
    /// the master key lives in a permission-restricted file instead.
    FileFallback,
    /// Neither the keychain nor the fallback file could be used reliably;
    /// an ephemeral in-memory key is in play and secrets will not survive a
    /// restart, or a previously stored key was corrupt and was replaced.
    Unavailable,
}

static CIPHER: OnceLock<SecretCipher> = OnceLock::new();
static STATUS: OnceLock<KeychainStatus> = OnceLock::new();

/// Process-global cipher backed by the OS keychain (falling back to a file,
/// see [`load`]). Initialized lazily on first use.
pub fn cipher() -> &'static SecretCipher {
    CIPHER.get_or_init(|| {
        let (cipher, status) = load();
        let _ = STATUS.set(status);
        cipher
    })
}

/// Where the master key came from. Forces [`cipher`] to initialize first so
/// the two globals are always set together.
pub fn keychain_status() -> KeychainStatus {
    let _ = cipher();
    STATUS.get().copied().unwrap_or(KeychainStatus::Unavailable)
}

fn fallback_path() -> PathBuf {
    crate::paths::state_dir().join("secret.key")
}

fn generate_key_bytes() -> [u8; 32] {
    let key = Key::<XChaCha20Poly1305>::generate();
    key.as_slice()
        .try_into()
        .expect("XChaCha20Poly1305 key is 32 bytes")
}

/// Load the 32-byte master key from the OS keychain, provisioning one on
/// first run, and falling back to a permission-restricted file when the
/// keychain is unavailable or its stored contents are unusable.
///
/// **Never panics** — every keyring/file error is swallowed into a fallback
/// so a broken keychain or read-only disk never blocks startup.
fn load() -> (SecretCipher, KeychainStatus) {
    match Entry::new(KEYCHAIN_SERVICE, KEYCHAIN_ACCOUNT).and_then(|e| e.get_secret()) {
        Ok(bytes) => match <[u8; 32]>::try_from(bytes.as_slice()) {
            Ok(key) => (SecretCipher::from_key(key), KeychainStatus::Ok),
            Err(_) => {
                tracing::warn!(
                    "keychain-stored secret has unexpected length; treating as corrupt \
                     and falling back to file"
                );
                let (cipher, _) = load_from_file(&fallback_path());
                (cipher, KeychainStatus::Unavailable)
            }
        },
        Err(KeyringError::NoEntry) => provision_keychain(),
        Err(KeyringError::NoStorageAccess(err)) | Err(KeyringError::PlatformFailure(err)) => {
            tracing::warn!("OS keychain unavailable ({err}); using file fallback");
            load_from_file(&fallback_path())
        }
        Err(err) => {
            tracing::warn!("unexpected keychain error ({err}); using file fallback");
            load_from_file(&fallback_path())
        }
    }
}

/// The keychain has no entry yet: generate a fresh key and store it there.
/// Falls back to the file if the keychain write itself fails.
fn provision_keychain() -> (SecretCipher, KeychainStatus) {
    let key_bytes = generate_key_bytes();
    match Entry::new(KEYCHAIN_SERVICE, KEYCHAIN_ACCOUNT).and_then(|e| e.set_secret(&key_bytes)) {
        Ok(()) => (SecretCipher::from_key(key_bytes), KeychainStatus::Ok),
        Err(err) => {
            tracing::warn!("failed to store new key in keychain ({err}); using file fallback");
            load_from_file(&fallback_path())
        }
    }
}

/// Read (or generate + persist) the 32-byte key at `path`. This backs both
/// the real file fallback (called with [`fallback_path`]) and the tests
/// below (called with a temp path), so the keychain never needs to be
/// touched to exercise the file-storage logic.
fn load_from_file(path: &Path) -> (SecretCipher, KeychainStatus) {
    match read_or_create_key_file(path) {
        Ok(key_bytes) => (
            SecretCipher::from_key(key_bytes),
            KeychainStatus::FileFallback,
        ),
        Err(err) => {
            tracing::error!(
                "secret key file fallback failed ({err}); using an ephemeral in-memory key \
                 (secrets will not persist across restarts)"
            );
            (
                SecretCipher::from_key(generate_key_bytes()),
                KeychainStatus::Unavailable,
            )
        }
    }
}

fn read_or_create_key_file(path: &Path) -> std::io::Result<[u8; 32]> {
    if let Ok(bytes) = std::fs::read(path) {
        match <[u8; 32]>::try_from(bytes.as_slice()) {
            Ok(key) => return Ok(key),
            Err(_) => tracing::warn!("secret key file has unexpected length; regenerating"),
        }
    }
    let key_bytes = generate_key_bytes();
    write_key_file(path, &key_bytes)?;
    Ok(key_bytes)
}

fn write_key_file(path: &Path, key_bytes: &[u8; 32]) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(path, key_bytes)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut perms = std::fs::metadata(path)?.permissions();
        perms.set_mode(0o600);
        std::fs::set_permissions(path, perms)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    // Fixed-key helper. Renamed off `cipher` so it can't silently shadow the
    // module-scope `pub fn cipher()` global accessor via `use super::*`.
    fn test_cipher() -> SecretCipher {
        SecretCipher::from_key([7u8; 32])
    }

    #[test]
    fn roundtrip_and_sentinel() {
        let c = test_cipher();
        let enc = c.encrypt("sk-secret");
        assert!(enc.starts_with("enc:v1:"), "got {enc}");
        assert_ne!(enc, "sk-secret");
        assert_eq!(c.decrypt(&enc).unwrap(), "sk-secret");
    }

    #[test]
    fn nonce_is_fresh_each_call() {
        let c = test_cipher();
        assert_ne!(
            c.encrypt("x"),
            c.encrypt("x"),
            "nonce must be random per call"
        );
    }

    #[test]
    fn plaintext_passes_through() {
        let c = test_cipher();
        assert_eq!(c.decrypt("sk-legacy-plain").unwrap(), "sk-legacy-plain");
        assert_eq!(c.decrypt("").unwrap(), "");
    }

    #[test]
    fn tamper_and_wrong_key_fail() {
        let enc = test_cipher().encrypt("secret");
        assert!(SecretCipher::from_key([9u8; 32]).decrypt(&enc).is_err());
        let mut bad = enc.clone();
        bad.push('A'); // corrupt the base64 tail
        assert!(test_cipher().decrypt(&bad).is_err());
    }

    #[test]
    fn unknown_enc_version_errors_not_passthrough() {
        assert!(test_cipher().decrypt("enc:v2:whatever").is_err());
    }

    #[test]
    fn unknown_version_with_multibyte_char_does_not_panic() {
        // `enc:v2:` is 7 bytes, `1234` is 4 (through byte 10), so the 2-byte
        // 'é' occupies bytes 11-12 — byte index 12 lands *inside* it and is not
        // a char boundary. The error arm must not raw-slice `&s[..12]` (panics);
        // it must return Err, not panic.
        let c = SecretCipher::from_key([7u8; 32]);
        assert!(c.decrypt("enc:v2:1234é_padding_here").is_err());
    }

    #[test]
    fn load_from_file_generates_and_persists_a_32_byte_key() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("secret.key");

        let (cipher, status) = load_from_file(&path);

        assert_eq!(status, KeychainStatus::FileFallback);
        let bytes = std::fs::read(&path).unwrap();
        assert_eq!(bytes.len(), 32);
        let enc = cipher.encrypt("hello");
        assert_eq!(cipher.decrypt(&enc).unwrap(), "hello");
    }

    #[test]
    fn load_from_file_reuses_the_same_key_on_a_second_call() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("secret.key");

        let (first, _) = load_from_file(&path);
        let enc = first.encrypt("round-trip");

        let (second, status) = load_from_file(&path);

        assert_eq!(status, KeychainStatus::FileFallback);
        assert_eq!(second.decrypt(&enc).unwrap(), "round-trip");
    }

    #[test]
    fn file_fallback_never_panics_on_any_input() {
        // The file-fallback seam must be panic-safe for the three shapes it
        // can meet on disk: absent, wrong-length (corrupt), and valid. This
        // is the hermetic proxy for `load()`'s infallibility — we exercise it
        // through the injected temp-path seam ONLY, never the real `load()`
        // or the global accessors, so `cargo test` never touches the OS
        // keychain or the production state dir.
        //
        // (a) fresh path — nothing on disk yet.
        let dir = tempfile::tempdir().unwrap();
        let fresh = dir.path().join("fresh.key");
        let (c, status) = load_from_file(&fresh);
        assert_eq!(status, KeychainStatus::FileFallback);
        assert_eq!(c.decrypt(&c.encrypt("a")).unwrap(), "a");

        // (b) wrong-length file — must be treated as corrupt and regenerated,
        // not panic. 10 bytes ≠ 32.
        let short = dir.path().join("short.key");
        std::fs::write(&short, [1u8; 10]).unwrap();
        let (c, status) = load_from_file(&short);
        assert_eq!(status, KeychainStatus::FileFallback);
        assert_eq!(std::fs::read(&short).unwrap().len(), 32); // regenerated
        assert_eq!(c.decrypt(&c.encrypt("b")).unwrap(), "b");

        // (c) exactly 32 bytes already present — reused as-is.
        let exact = dir.path().join("exact.key");
        std::fs::write(&exact, [2u8; 32]).unwrap();
        let (c, status) = load_from_file(&exact);
        assert_eq!(status, KeychainStatus::FileFallback);
        assert_eq!(std::fs::read(&exact).unwrap(), [2u8; 32]); // untouched
        assert_eq!(c.decrypt(&c.encrypt("c")).unwrap(), "c");
    }

    // The global-accessor ordering (STATUS.set happens inside
    // CIPHER.get_or_init's closure, and keychain_status() forces cipher()
    // first) was verified by inspection; it is deliberately NOT unit-tested,
    // because `cipher()`/`keychain_status()`/`load()` touch the real OS
    // keychain and the production state dir, which a `cargo test` run must
    // never mutate.
}
