//! PKCE (RFC 7636) code verifier/challenge + state, S256.
use base64::Engine;
use sha2::{Digest, Sha256};

pub struct Pkce {
    pub verifier: String,
    pub challenge: String,
    pub state: String,
}

fn random_32() -> [u8; 32] {
    let mut b = [0u8; 32];
    b[..16].copy_from_slice(uuid::Uuid::new_v4().as_bytes());
    b[16..].copy_from_slice(uuid::Uuid::new_v4().as_bytes());
    b
}

fn b64url(bytes: &[u8]) -> String {
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(bytes)
}

pub fn generate() -> Pkce {
    let verifier = b64url(&random_32());
    let challenge = b64url(&Sha256::digest(verifier.as_bytes()));
    let state = b64url(&random_32());
    Pkce {
        verifier,
        challenge,
        state,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use base64::Engine;
    use sha2::{Digest, Sha256};

    #[test]
    fn challenge_is_s256_of_verifier_and_fields_are_urlsafe_nopad() {
        let p = generate();
        // verifier decodes to 32 bytes
        let vb = base64::engine::general_purpose::URL_SAFE_NO_PAD
            .decode(&p.verifier)
            .unwrap();
        assert_eq!(vb.len(), 32);
        // challenge == base64url(sha256(verifier_ascii))
        let want = base64::engine::general_purpose::URL_SAFE_NO_PAD
            .encode(Sha256::digest(p.verifier.as_bytes()));
        assert_eq!(p.challenge, want);
        // no padding / url-safe alphabet
        assert!(
            !p.verifier.contains('=') && !p.verifier.contains('+') && !p.verifier.contains('/')
        );
        assert_eq!(
            base64::engine::general_purpose::URL_SAFE_NO_PAD
                .decode(&p.state)
                .unwrap()
                .len(),
            32
        );
    }

    #[test]
    fn each_generate_is_unique() {
        assert_ne!(generate().verifier, generate().verifier);
    }
}
