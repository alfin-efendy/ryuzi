//! Value-level encryption of stored secrets. Secrets are tagged
//! `enc:v1:<base64url(nonce‖ciphertext)>`; anything without the `enc:` prefix
//! is treated as legacy plaintext and passed through unchanged, so no schema
//! migration is needed — legacy rows upgrade lazily (and via a startup sweep).
use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine as _};
use chacha20poly1305::{
    aead::{Aead, Generate, KeyInit},
    XChaCha20Poly1305, XNonce,
};

const ENC_PREFIX: &str = "enc:v1:";

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
        let b64 = s
            .strip_prefix(ENC_PREFIX)
            .ok_or_else(|| anyhow::anyhow!("unknown secret encoding: {}", &s[..s.len().min(12)]))?;
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

#[cfg(test)]
mod tests {
    use super::*;

    fn cipher() -> SecretCipher {
        SecretCipher::from_key([7u8; 32])
    }

    #[test]
    fn roundtrip_and_sentinel() {
        let c = cipher();
        let enc = c.encrypt("sk-secret");
        assert!(enc.starts_with("enc:v1:"), "got {enc}");
        assert_ne!(enc, "sk-secret");
        assert_eq!(c.decrypt(&enc).unwrap(), "sk-secret");
    }

    #[test]
    fn nonce_is_fresh_each_call() {
        let c = cipher();
        assert_ne!(
            c.encrypt("x"),
            c.encrypt("x"),
            "nonce must be random per call"
        );
    }

    #[test]
    fn plaintext_passes_through() {
        let c = cipher();
        assert_eq!(c.decrypt("sk-legacy-plain").unwrap(), "sk-legacy-plain");
        assert_eq!(c.decrypt("").unwrap(), "");
    }

    #[test]
    fn tamper_and_wrong_key_fail() {
        let enc = cipher().encrypt("secret");
        assert!(SecretCipher::from_key([9u8; 32]).decrypt(&enc).is_err());
        let mut bad = enc.clone();
        bad.push('A'); // corrupt the base64 tail
        assert!(cipher().decrypt(&bad).is_err());
    }

    #[test]
    fn unknown_enc_version_errors_not_passthrough() {
        assert!(cipher().decrypt("enc:v2:whatever").is_err());
    }
}
