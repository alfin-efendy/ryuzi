//! Value-level encryption of stored secrets. Secrets are tagged
//! `enc:v1:<base64url(nonce‖ciphertext)>`; anything without the `enc:` prefix
//! is treated as legacy plaintext and passed through unchanged, so no schema
//! migration is needed — legacy rows upgrade lazily (and via a startup sweep).
use crate::store::Store;
use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine as _};
use chacha20poly1305::{
    aead::{Aead, Generate, Key, KeyInit},
    XChaCha20Poly1305, XNonce,
};
use keyring::{Entry, Error as KeyringError};
use rusqlite::params;
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

/// Encrypt a single secret value with the process-global cipher.
pub fn encrypt_field(plain: &str) -> String {
    cipher().encrypt(plain)
}

/// Decrypt a single secret value with the process-global cipher. Legacy
/// (non-`enc:`-prefixed) plaintext passes through unchanged.
pub fn decrypt_field(s: &str) -> anyhow::Result<String> {
    cipher().decrypt(s)
}

/// Encrypt every secret field of a [`ConnectionData`] in place: `api_key`,
/// `access_token`, `refresh_token` (each individually), and `provider_specific`
/// (serialized whole, since the Kiro `clientSecret` is nested inside it).
/// Idempotent — a value that is already `enc:`-prefixed (or already an
/// encrypted `provider_specific` string) is left untouched, so re-saving an
/// already-encrypted row never double-encrypts.
pub fn encrypt_conn_data(d: &mut crate::llm_router::connections::ConnectionData) {
    for v in [&mut d.api_key, &mut d.access_token, &mut d.refresh_token]
        .into_iter()
        .flatten()
    {
        if !v.starts_with("enc:") {
            *v = encrypt_field(v);
        }
    }
    if let Some(v) = &d.provider_specific {
        let already_encrypted = matches!(v, serde_json::Value::String(s) if s.starts_with("enc:"));
        if !already_encrypted {
            if let Ok(json) = serde_json::to_string(v) {
                d.provider_specific = Some(serde_json::Value::String(encrypt_field(&json)));
            }
        }
    }
}

/// Reverse of [`encrypt_conn_data`]: decrypt every secret field in place.
///
/// **Decrypt-error handling:** if decryption fails (wrong/lost key, or a
/// corrupt value), the ciphertext is left in place — never blanked and never
/// a panic — and `needs_relogin` is set so the row is flagged for the user to
/// re-authenticate (Task 5 refines the locked/lost-key UX further). A
/// non-string `provider_specific` (legacy plaintext object) is left as-is.
pub fn decrypt_conn_data(d: &mut crate::llm_router::connections::ConnectionData) {
    let mut decrypt_failed = false;
    for v in [&mut d.api_key, &mut d.access_token, &mut d.refresh_token]
        .into_iter()
        .flatten()
    {
        match decrypt_field(v) {
            Ok(plain) => *v = plain,
            Err(err) => {
                tracing::warn!("failed to decrypt connection secret field: {err}");
                decrypt_failed = true;
            }
        }
    }
    if decrypt_failed {
        d.needs_relogin = Some(true);
    }
    if let Some(serde_json::Value::String(s)) = &d.provider_specific {
        match decrypt_field(s) {
            Ok(plain) => match serde_json::from_str::<serde_json::Value>(&plain) {
                Ok(v) => d.provider_specific = Some(v),
                Err(err) => {
                    tracing::warn!("failed to parse decrypted provider_specific json: {err}");
                    d.needs_relogin = Some(true);
                }
            },
            Err(err) => {
                tracing::warn!("failed to decrypt connection provider_specific: {err}");
                d.needs_relogin = Some(true);
            }
        }
    }
}

/// One-time (and idempotent — safe to call on every boot) upgrade path: force
/// the master-key globals ([`cipher`]/[`keychain_status`]) to initialize,
/// then, when the keychain is usable, sweep every `provider_connections` and
/// `endpoint_keys` row and encrypt any secret field that is still legacy
/// plaintext. Rows that are already fully encrypted are left completely
/// untouched — no decrypt→re-encrypt churn — which is what makes a second
/// call a true no-op (no nonce changes on already-encrypted rows).
///
/// **Atomicity:** each row is swept with its own UPDATE. A failure partway
/// through (a single row's read/write error) is logged and that one row is
/// skipped — it is picked up again on the next start. The whole sweep is
/// deliberately NOT one transaction, so one bad row can never block or
/// half-apply changes to the others.
///
/// **Degraded state:** when [`keychain_status`] is
/// [`KeychainStatus::Unavailable`], the sweep is skipped entirely. Existing
/// `enc:` rows are effectively locked — they will fail to decrypt against
/// whatever key is in play, and [`decrypt_conn_data`] already surfaces that
/// as `needs_relogin` (never a crash, never a silently blanked secret). New
/// secrets keep being written by the normal `encrypt_field`/
/// `encrypt_conn_data` call sites (plaintext for now, since the process-wide
/// cipher itself falls back to an ephemeral key in this state) until the
/// keychain recovers on a later start.
pub async fn init_and_sweep(store: &Store) {
    let _ = cipher();
    let status = keychain_status();
    if !should_sweep(status) {
        tracing::warn!(
            "keychain unavailable at startup; skipping the encryption sweep \
             (existing enc: rows stay locked — decrypt failures flag needs_relogin \
             instead of crashing or dropping data)"
        );
        return;
    }
    sweep_connections(store).await;
    sweep_endpoint_keys(store).await;
}

/// Whether [`init_and_sweep`] should run its sweep for a given keychain
/// status. Pure so the "never sweep when Unavailable" rule is directly
/// unit-testable without touching the process-global cipher/keychain.
fn should_sweep(status: KeychainStatus) -> bool {
    !matches!(status, KeychainStatus::Unavailable)
}

/// True when any secret field of `d` is still legacy plaintext, i.e. would be
/// changed by [`encrypt_conn_data`]. The sweep uses this to skip rows that
/// are already fully encrypted, so re-running it never churns nonces.
fn conn_data_needs_encryption(d: &crate::llm_router::connections::ConnectionData) -> bool {
    for v in [&d.api_key, &d.access_token, &d.refresh_token]
        .into_iter()
        .flatten()
    {
        if !v.starts_with("enc:") {
            return true;
        }
    }
    if let Some(v) = &d.provider_specific {
        let already_encrypted = matches!(v, serde_json::Value::String(s) if s.starts_with("enc:"));
        if !already_encrypted {
            return true;
        }
    }
    false
}

/// Sweep `provider_connections`: encrypt any row whose `data` still holds a
/// plaintext secret field. See [`init_and_sweep`] for the atomicity/idempotency
/// contract.
async fn sweep_connections(store: &Store) {
    let rows: Vec<(String, String)> = match store
        .with_conn(|c| -> rusqlite::Result<Vec<(String, String)>> {
            let mut stmt = c.prepare("SELECT id, data FROM provider_connections")?;
            let items = stmt
                .query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            Ok(items)
        })
        .await
    {
        Ok(rows) => rows,
        Err(err) => {
            tracing::warn!("secret sweep: failed to list provider_connections: {err}");
            return;
        }
    };

    for (id, raw) in rows {
        let mut data: crate::llm_router::connections::ConnectionData =
            match serde_json::from_str(&raw) {
                Ok(d) => d,
                Err(err) => {
                    tracing::warn!(
                        "secret sweep: skipping connection {id}, unparseable data json: {err}"
                    );
                    continue;
                }
            };
        if !conn_data_needs_encryption(&data) {
            continue; // already fully encrypted — leave untouched
        }
        encrypt_conn_data(&mut data);
        let new_json = match serde_json::to_string(&data) {
            Ok(j) => j,
            Err(err) => {
                tracing::warn!("secret sweep: failed to serialize connection {id}: {err}");
                continue;
            }
        };
        let row_id = id.clone();
        if let Err(err) = store
            .with_conn(move |c| {
                c.execute(
                    "UPDATE provider_connections SET data=?2 WHERE id=?1",
                    params![row_id, new_json],
                )
                .map(|_| ())
            })
            .await
        {
            tracing::warn!(
                "secret sweep: failed to persist encrypted data for connection {id}: {err}"
            );
        }
    }
}

/// Sweep `endpoint_keys`: encrypt any row whose `key` column is still
/// plaintext. See [`init_and_sweep`] for the atomicity/idempotency contract.
async fn sweep_endpoint_keys(store: &Store) {
    let rows: Vec<(String, String)> = match store
        .with_conn(|c| -> rusqlite::Result<Vec<(String, String)>> {
            let mut stmt = c.prepare("SELECT id, key FROM endpoint_keys")?;
            let items = stmt
                .query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            Ok(items)
        })
        .await
    {
        Ok(rows) => rows,
        Err(err) => {
            tracing::warn!("secret sweep: failed to list endpoint_keys: {err}");
            return;
        }
    };

    for (id, raw) in rows {
        if raw.starts_with("enc:") {
            continue; // already encrypted — leave untouched
        }
        let encrypted = encrypt_field(&raw);
        let row_id = id.clone();
        if let Err(err) = store
            .with_conn(move |c| {
                c.execute(
                    "UPDATE endpoint_keys SET key=?2 WHERE id=?1",
                    params![row_id, encrypted],
                )
                .map(|_| ())
            })
            .await
        {
            tracing::warn!("secret sweep: failed to persist encrypted key {id}: {err}");
        }
    }
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

    // --- init_and_sweep -----------------------------------------------
    //
    // Note: `init_and_sweep` necessarily calls the process-global `cipher()`
    // (that's the whole point — it forces the master-key globals to init).
    // This is no new risk: `add_connection`/`create_key` already call
    // `cipher()` transitively via `encrypt_conn_data`/`encrypt_field` in the
    // `connections`/`keys` test suites, so the global has been exercised
    // under `cargo test` since Task 4. On this dev box (and CI) that
    // resolves deterministically to the file fallback for the lifetime of
    // the test binary.

    async fn mem_store() -> Store {
        let tmp = tempfile::NamedTempFile::new().unwrap();
        let (_, path) = tmp.keep().unwrap();
        Store::open(&path).await.unwrap()
    }

    async fn insert_raw_connection(store: &Store, id: &str, raw_data_json: &str) {
        let id = id.to_string();
        let raw = raw_data_json.to_string();
        store
            .with_conn(move |c| {
                c.execute(
                    "INSERT INTO provider_connections(id,provider,auth_type,label,priority,enabled,data,created_at,updated_at) \
                     VALUES (?1,'openai','api_key','L',0,1,?2,1,1)",
                    params![id, raw],
                )
                .map(|_| ())
            })
            .await
            .unwrap();
    }

    async fn insert_raw_endpoint_key(store: &Store, id: &str, raw_key: &str) {
        let id = id.to_string();
        let raw = raw_key.to_string();
        store
            .with_conn(move |c| {
                c.execute(
                    "INSERT INTO endpoint_keys(id,name,key,created_at,last_used_at) VALUES (?1,'dev',?2,1,NULL)",
                    params![id, raw],
                )
                .map(|_| ())
            })
            .await
            .unwrap();
    }

    async fn raw_connection_data(store: &Store, id: &str) -> String {
        let id = id.to_string();
        store
            .with_conn(move |c| {
                c.query_row(
                    "SELECT data FROM provider_connections WHERE id=?1",
                    params![id],
                    |r| r.get(0),
                )
            })
            .await
            .unwrap()
    }

    async fn raw_endpoint_key(store: &Store, id: &str) -> String {
        let id = id.to_string();
        store
            .with_conn(move |c| {
                c.query_row(
                    "SELECT key FROM endpoint_keys WHERE id=?1",
                    params![id],
                    |r| r.get(0),
                )
            })
            .await
            .unwrap()
    }

    #[test]
    fn sweep_runs_only_when_keychain_ok_or_file_fallback() {
        assert!(should_sweep(KeychainStatus::Ok));
        assert!(should_sweep(KeychainStatus::FileFallback));
        assert!(!should_sweep(KeychainStatus::Unavailable));
    }

    #[tokio::test]
    async fn sweep_encrypts_legacy_plaintext_rows() {
        let store = mem_store().await;
        // Raw plaintext inserted directly via `with_conn`, bypassing the
        // encrypting write paths (`add_connection`/`create_key`) entirely —
        // this simulates a pre-F3b database.
        insert_raw_connection(&store, "c1", r#"{"apiKey":"sk-legacy-plain"}"#).await;
        insert_raw_endpoint_key(&store, "k1", "ryz-legacy-plain").await;

        init_and_sweep(&store).await;

        let raw_data = raw_connection_data(&store, "c1").await;
        assert!(raw_data.contains("enc:v1:"), "got {raw_data}");
        assert!(!raw_data.contains("sk-legacy-plain"), "got {raw_data}");

        let raw_key = raw_endpoint_key(&store, "k1").await;
        assert!(raw_key.starts_with("enc:v1:"), "got {raw_key}");

        // Reading back through the normal (decrypting) read paths still
        // yields the original plaintext.
        let conns = crate::llm_router::connections::list_connections(&store)
            .await
            .unwrap();
        assert_eq!(conns[0].data.api_key.as_deref(), Some("sk-legacy-plain"));
        let keys = crate::llm_router::keys::list_keys(&store).await.unwrap();
        assert_eq!(keys[0].key, "ryz-legacy-plain");
    }

    #[tokio::test]
    async fn sweep_is_idempotent() {
        let store = mem_store().await;
        insert_raw_connection(&store, "c1", r#"{"apiKey":"sk-legacy-plain"}"#).await;
        insert_raw_endpoint_key(&store, "k1", "ryz-legacy-plain").await;

        init_and_sweep(&store).await;
        let raw_data_1 = raw_connection_data(&store, "c1").await;
        let raw_key_1 = raw_endpoint_key(&store, "k1").await;

        // A second sweep must be a true no-op: same ciphertext, no nonce
        // churn — proves the needs-encryption pre-check works.
        init_and_sweep(&store).await;
        let raw_data_2 = raw_connection_data(&store, "c1").await;
        let raw_key_2 = raw_endpoint_key(&store, "k1").await;

        assert_eq!(
            raw_data_1, raw_data_2,
            "already-encrypted connection data must not be rewritten"
        );
        assert_eq!(
            raw_key_1, raw_key_2,
            "already-encrypted endpoint key must not be rewritten"
        );
    }

    #[tokio::test]
    async fn sweep_skips_a_row_that_is_already_fully_encrypted() {
        let store = mem_store().await;
        let mut data = crate::llm_router::connections::ConnectionData {
            api_key: Some("sk-already".into()),
            provider_specific: Some(serde_json::json!({"clientSecret": "shh"})),
            ..Default::default()
        };
        encrypt_conn_data(&mut data);
        let json = serde_json::to_string(&data).unwrap();
        insert_raw_connection(&store, "c1", &json).await;
        insert_raw_endpoint_key(&store, "k1", &encrypt_field("ryz-already")).await;

        let before_data = raw_connection_data(&store, "c1").await;
        let before_key = raw_endpoint_key(&store, "k1").await;

        init_and_sweep(&store).await;

        assert_eq!(
            before_data,
            raw_connection_data(&store, "c1").await,
            "already-encrypted connection row must not be touched"
        );
        assert_eq!(
            before_key,
            raw_endpoint_key(&store, "k1").await,
            "already-encrypted endpoint key row must not be touched"
        );
    }

    #[test]
    fn locked_row_when_keychain_unavailable() {
        // A "key-lost"/wrong-key scenario: the ciphertext was produced with a
        // key OTHER than whatever the process-global cipher resolves to in
        // this test binary. `decrypt_conn_data` must leave the ciphertext in
        // place (never blank it, never panic) and flag `needs_relogin` — the
        // honest, non-bricking behavior for a locked/lost-key row.
        let foreign = SecretCipher::from_key([42u8; 32]);
        let ciphertext = foreign.encrypt("sk-locked");
        let mut d = crate::llm_router::connections::ConnectionData {
            api_key: Some(ciphertext.clone()),
            ..Default::default()
        };
        decrypt_conn_data(&mut d);
        assert_eq!(
            d.api_key.as_deref(),
            Some(ciphertext.as_str()),
            "undecryptable ciphertext must be left in place, not blanked"
        );
        assert_eq!(d.needs_relogin, Some(true));
    }
}
