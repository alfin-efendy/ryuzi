//! TLS material for the remote-runner control plane: a self-signed
//! certificate + private key, persisted under the control directory so a
//! same-directory daemon restart reuses the same identity — and therefore
//! the same fingerprint that already-paired remote clients pinned (mirrors
//! `control_token::write_token`'s "reuse what's on disk" stability goal).
//!
//! # Trust model: certificate pinning, not a CA chain
//! There is no certificate authority here: Phase-3 remote clients pin the
//! leaf certificate itself (TOFU), not a chain to a trusted root. The SAN is
//! the permissive placeholder `"localhost"` because it plays no role in that
//! trust decision — only the fingerprint does.
//!
//! [`fingerprint_cert_der`] hashes the **whole certificate DER** (SHA-256,
//! base64 standard-alphabet), not just the SubjectPublicKeyInfo
//! sub-structure. This is the simplest, most unambiguous "certificate pin"
//! shape (no need to locate/extract the SPKI bytes inside the DER) and is
//! the standard meaning of "cert pin" as opposed to "public-key pin". The
//! Phase-3 client MUST compute its pin the exact same way — SHA-256 over the
//! full leaf certificate DER, base64 standard alphabet — or fingerprints
//! will never match.
//!
//! # ring-only
//! Certificate generation goes through `rcgen` with `default-features =
//! false, features = ["ring", "pem"]` — rcgen's default features pull in
//! `aws_lc_rs`, which is banned workspace-wide (see `crates/core/Cargo.toml`
//! and the release cross-compile notes there). `rcgen::KeyPair::generate()`
//! / `generate_simple_self_signed()` are gated on rcgen's `crypto` feature
//! (which `ring` implies), not on `aws_lc_rs`, so they work unmodified under
//! the ring-only feature set — verified by compiling this module and by
//! `cargo tree -p ryuzi-core -i aws-lc-rs` / `-i aws-lc-sys` reporting no
//! match.

use base64::engine::general_purpose::STANDARD as BASE64;
use base64::Engine as _;
use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};

/// A TLS identity: DER-encoded certificate + private key, plus the pinned
/// fingerprint of the certificate (see module docs for what "fingerprint"
/// means here).
pub struct TlsMaterial {
    pub cert_der: Vec<u8>,
    pub key_der: Vec<u8>,
    pub fingerprint: String,
}

fn cert_path(dir: &Path) -> PathBuf {
    dir.join("tls_cert.pem")
}

fn key_path(dir: &Path) -> PathBuf {
    dir.join("tls_key.pem")
}

/// Load the persisted self-signed cert/key from `dir` if both PEM files are
/// present, otherwise generate a fresh self-signed cert (rcgen, ring,
/// ECDSA P-256) and persist it there before returning.
///
/// Persistence is what makes the fingerprint stable across daemon restarts
/// on the same control directory — exactly the property Phase-3 pairing
/// depends on (a client that already pinned a fingerprint must keep trusting
/// the daemon after a restart).
pub fn load_or_generate(dir: &Path) -> anyhow::Result<TlsMaterial> {
    let cert_file = cert_path(dir);
    let key_file = key_path(dir);

    if let (Ok(cert_pem), Ok(key_pem)) = (
        std::fs::read_to_string(&cert_file),
        std::fs::read_to_string(&key_file),
    ) {
        let cert_der = pem_to_der(&cert_pem)?;
        let key_der = pem_to_der(&key_pem)?;
        if pair_is_valid(&cert_der, &key_der) {
            let fingerprint = fingerprint_cert_der(&cert_der);
            return Ok(TlsMaterial {
                cert_der,
                key_der,
                fingerprint,
            });
        }
        // A mismatched pair (e.g. a partial-write failure left a fresh cert
        // paired with a stale key, or one file was deleted/replaced out from
        // under us) would otherwise be silently trusted and only fail much
        // later at TLS handshake time. Fall through to regeneration instead.
        eprintln!(
            "tls: cert/key pair on disk at {} failed validation, regenerating",
            dir.display()
        );
    }

    // SAN is a placeholder — clients pin by fingerprint, not hostname.
    let rcgen::CertifiedKey { cert, key_pair } =
        rcgen::generate_simple_self_signed(vec!["localhost".to_string()])?;
    let cert_pem = cert.pem();
    let key_pem = key_pair.serialize_pem();

    std::fs::create_dir_all(dir)?;
    std::fs::write(&cert_file, &cert_pem)?;
    write_key_pem(&key_file, &key_pem)?;

    let cert_der = cert.der().to_vec();
    let key_der = key_pair.serialize_der();
    let fingerprint = fingerprint_cert_der(&cert_der);

    Ok(TlsMaterial {
        cert_der,
        key_der,
        fingerprint,
    })
}

/// Build a ring-provider rustls `ServerConfig` from `material`'s cert/key,
/// with ALPN protocols set to `h2` + `http/1.1`.
///
/// The single place the ring `ServerConfig` is actually constructed for a
/// real TLS listener — [`pair_is_valid`] and `daemon_cmd::start_control_api`
/// (P2-7) both funnel through this rather than duplicating the
/// `builder_with_provider(...).with_safe_default_protocol_versions()...`
/// dance. `pub` (not `pub(crate)`) so `tests/control_api.rs` (P2-9, an
/// external integration-test crate that can only see `pub` items) can hand a
/// genuine `Arc<ServerConfig>` to `serve::ServeOpts` without duplicating this
/// construction — the same reasoning [`resolve_bind`] already applies.
///
/// ALPN matters because `axum_server`'s `RustlsConfig::from_config` serves
/// whatever protocols are negotiated — without `alpn_protocols` set, some
/// HTTP/2-capable clients fail to negotiate a protocol at all. rustls 0.23's
/// builder finalizes the config before ALPN can be supplied to it, so this
/// sets `alpn_protocols` on the built `ServerConfig` afterward (it's a plain
/// public field, not part of the builder chain).
pub fn server_config(
    material: &TlsMaterial,
) -> anyhow::Result<std::sync::Arc<rustls::ServerConfig>> {
    let cert = rustls::pki_types::CertificateDer::from(material.cert_der.clone());
    let key = rustls::pki_types::PrivateKeyDer::try_from(material.key_der.clone())
        .map_err(|e| anyhow::anyhow!("tls: invalid private key DER: {e}"))?;
    let mut config = rustls::ServerConfig::builder_with_provider(std::sync::Arc::new(
        rustls::crypto::ring::default_provider(),
    ))
    .with_safe_default_protocol_versions()?
    .with_no_client_auth()
    .with_single_cert(vec![cert], key)?;
    config.alpn_protocols = vec![b"h2".to_vec(), b"http/1.1".to_vec()];
    Ok(std::sync::Arc::new(config))
}

/// Whether `cert_der` + `key_der` form a usable TLS server identity — the
/// exact check is "can we build a rustls `ServerConfig` from them", since
/// that's the operation a real consumer performs. Delegates to
/// [`server_config`] so the ring-`ServerConfig`-build logic lives in one
/// place.
fn pair_is_valid(cert_der: &[u8], key_der: &[u8]) -> bool {
    let material = TlsMaterial {
        cert_der: cert_der.to_vec(),
        key_der: key_der.to_vec(),
        fingerprint: String::new(),
    };
    server_config(&material).is_ok()
}

/// `resolve_bind`'s return shape: bind IP, optional TLS server config, URL
/// scheme, and cert fingerprint (if any). Factored into a named alias
/// (rather than an inline 4-tuple type) to satisfy clippy's
/// `type_complexity` lint.
pub type ResolvedBind = (
    std::net::IpAddr,
    Option<std::sync::Arc<rustls::ServerConfig>>,
    &'static str,
    Option<String>,
);

/// The pure listen-address decision behind `ryuzi-runner`'s
/// `daemon_cmd::start_control_api` (P2-7): given the raw `listen_addr`
/// setting value and the control directory, resolve the bind IP, an
/// optional TLS server config, the URL scheme, and the cert fingerprint (if
/// any). `pub` (not `pub(crate)`, unlike [`server_config`]) because the
/// runner crate — not `ryuzi-core` itself — is the one that calls this while
/// bringing up the control API.
///
/// - An unparsable `listen_addr` falls back to loopback (`127.0.0.1`) +
///   `http`, logging a warning — a bad setting value must never accidentally
///   widen the bind surface.
/// - A loopback address returns `tls: None`, scheme `"http"`, no
///   fingerprint — today's Cockpit-local behavior, unchanged.
/// - A non-loopback address loads/generates TLS material from `dir` (stable
///   across restarts — see [`load_or_generate`]) and builds a ring
///   `ServerConfig` via [`server_config`]; returns `"https"` and
///   `Some(fingerprint)`. If EITHER step fails, this returns `Err` — the
///   safety property this whole function exists to make testable is that
///   the caller must then refuse to start rather than silently falling back
///   to plaintext on a public interface.
pub fn resolve_bind(listen_addr: &str, dir: &Path) -> anyhow::Result<ResolvedBind> {
    let addr: std::net::IpAddr = listen_addr.parse().unwrap_or_else(|_| {
        tracing::warn!("daemon: invalid listen_addr {listen_addr:?}, defaulting to 127.0.0.1");
        std::net::Ipv4Addr::LOCALHOST.into()
    });

    if addr.is_loopback() {
        return Ok((addr, None, "http", None));
    }

    let material = load_or_generate(dir)?;
    let cfg = server_config(&material)?;
    Ok((addr, Some(cfg), "https", Some(material.fingerprint)))
}

/// Write the private key PEM 0600 on unix (same rationale as
/// `control_token::write_token`: same-user access is the trust boundary).
/// Plain write on Windows, mirroring that module's `#[cfg(not(unix))]` arm.
#[cfg(unix)]
fn write_key_pem(path: &Path, pem: &str) -> anyhow::Result<()> {
    use std::io::Write;
    use std::os::unix::fs::OpenOptionsExt;
    use std::os::unix::fs::PermissionsExt;
    let mut f = std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600) // applies at creation — file is born 0600
        .open(path)?;
    // A pre-existing file (e.g. left over with looser perms) keeps its old
    // mode across truncate; tighten it before any bytes land.
    f.set_permissions(std::fs::Permissions::from_mode(0o600))?;
    f.write_all(pem.as_bytes())?;
    Ok(())
}

#[cfg(not(unix))]
fn write_key_pem(path: &Path, pem: &str) -> anyhow::Result<()> {
    std::fs::write(path, pem)?;
    Ok(())
}

/// SHA-256 fingerprint of the whole certificate DER — see the module docs
/// "Trust model" section for why this hashes the entire cert DER rather than
/// just the SubjectPublicKeyInfo. Base64 standard-alphabet encoded.
pub fn fingerprint_cert_der(cert_der: &[u8]) -> String {
    BASE64.encode(Sha256::digest(cert_der))
}

/// Minimal PEM -> DER decoder: strips `-----BEGIN/END-----` delimiter lines
/// and base64-decodes the remaining body. `sha2`/`base64` are already direct
/// `ryuzi-core` deps and this format is trivial, so a dedicated PEM-parsing
/// crate isn't pulled in just for this.
fn pem_to_der(pem: &str) -> anyhow::Result<Vec<u8>> {
    let body: String = pem
        .lines()
        .filter(|line| !line.starts_with("-----"))
        .collect();
    Ok(BASE64.decode(body)?)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn load_or_generate_creates_material_and_pem_files() {
        let dir = tempfile::tempdir().unwrap();
        let material = load_or_generate(dir.path()).unwrap();
        assert!(!material.fingerprint.is_empty());
        assert!(!material.cert_der.is_empty());
        assert!(!material.key_der.is_empty());
        assert!(dir.path().join("tls_cert.pem").exists());
        assert!(dir.path().join("tls_key.pem").exists());
    }

    #[test]
    fn load_or_generate_is_stable_across_calls() {
        let dir = tempfile::tempdir().unwrap();
        let first = load_or_generate(dir.path()).unwrap();
        let second = load_or_generate(dir.path()).unwrap();
        assert_eq!(first.fingerprint, second.fingerprint);
        assert_eq!(first.cert_der, second.cert_der);
        assert_eq!(first.key_der, second.key_der);
    }

    #[test]
    fn fingerprint_is_deterministic_for_same_der() {
        let der = b"not a real cert DER, just bytes to hash".to_vec();
        assert_eq!(fingerprint_cert_der(&der), fingerprint_cert_der(&der));
    }

    #[test]
    fn fingerprint_differs_for_different_der() {
        assert_ne!(fingerprint_cert_der(b"one"), fingerprint_cert_der(b"two"));
    }

    #[test]
    fn mismatched_key_on_disk_triggers_regeneration() {
        let dir = tempfile::tempdir().unwrap();
        let first = load_or_generate(dir.path()).unwrap();

        // Corrupt the pair by overwriting the key file with a key from a
        // *different* identity — same shape as a real key PEM, but it does
        // not match the cert already on disk.
        let other_dir = tempfile::tempdir().unwrap();
        let other = load_or_generate(other_dir.path()).unwrap();
        assert_ne!(
            first.key_der, other.key_der,
            "test setup: the two generated keys must differ"
        );
        let other_key_pem = std::fs::read_to_string(key_path(other_dir.path())).unwrap();
        std::fs::write(key_path(dir.path()), &other_key_pem).unwrap();

        // Sanity: the pair really is now invalid (cert from `first`, key
        // from `other`) before we exercise the fix.
        let corrupted_key_der = pem_to_der(&other_key_pem).unwrap();
        assert!(!pair_is_valid(&first.cert_der, &corrupted_key_der));

        let regenerated = load_or_generate(dir.path()).unwrap();
        assert!(pair_is_valid(&regenerated.cert_der, &regenerated.key_der));
        // The stale key must not have leaked through.
        assert_ne!(regenerated.key_der, corrupted_key_der);

        // And the freshly-regenerated pair is now stable on disk.
        let third = load_or_generate(dir.path()).unwrap();
        assert_eq!(regenerated.fingerprint, third.fingerprint);
        assert_eq!(regenerated.cert_der, third.cert_der);
        assert_eq!(regenerated.key_der, third.key_der);
    }

    #[test]
    fn server_config_builds_ok_with_h2_and_http11_alpn() {
        let dir = tempfile::tempdir().unwrap();
        let material = load_or_generate(dir.path()).unwrap();
        let cfg = server_config(&material).unwrap();
        assert_eq!(
            cfg.alpn_protocols,
            vec![b"h2".to_vec(), b"http/1.1".to_vec()]
        );
    }

    // ---- resolve_bind: the non-loopback-requires-TLS decision (P2-7) ----

    #[test]
    fn resolve_bind_loopback_is_plain_http_with_no_tls() {
        let dir = tempfile::tempdir().unwrap();
        let (addr, tls, scheme, fp) = resolve_bind("127.0.0.1", dir.path()).unwrap();
        assert_eq!(addr, std::net::Ipv4Addr::LOCALHOST);
        assert!(tls.is_none());
        assert_eq!(scheme, "http");
        assert_eq!(fp, None);
        // Loopback must never touch the control dir for TLS material.
        assert!(!dir.path().join("tls_cert.pem").exists());
    }

    #[test]
    fn resolve_bind_ipv6_loopback_is_plain_http_with_no_tls() {
        let dir = tempfile::tempdir().unwrap();
        let (addr, tls, scheme, fp) = resolve_bind("::1", dir.path()).unwrap();
        assert!(addr.is_loopback());
        assert!(tls.is_none());
        assert_eq!(scheme, "http");
        assert_eq!(fp, None);
    }

    #[test]
    fn resolve_bind_non_loopback_builds_tls_and_returns_https_plus_fingerprint() {
        let dir = tempfile::tempdir().unwrap();
        let (addr, tls, scheme, fp) = resolve_bind("0.0.0.0", dir.path()).unwrap();
        assert_eq!(addr, std::net::Ipv4Addr::UNSPECIFIED);
        assert!(tls.is_some(), "non-loopback bind must carry a TLS config");
        assert_eq!(scheme, "https");
        let fp = fp.expect("non-loopback bind must carry a fingerprint");
        assert!(!fp.is_empty());
        // Same fingerprint the persisted material carries (stable identity).
        let material = load_or_generate(dir.path()).unwrap();
        assert_eq!(fp, material.fingerprint);
    }

    #[test]
    fn resolve_bind_unparsable_addr_defaults_to_loopback_http() {
        let dir = tempfile::tempdir().unwrap();
        let (addr, tls, scheme, fp) = resolve_bind("not-an-ip", dir.path()).unwrap();
        assert_eq!(addr, std::net::Ipv4Addr::LOCALHOST);
        assert!(tls.is_none());
        assert_eq!(scheme, "http");
        assert_eq!(fp, None);
        // The parse-failure fallback must not silently touch TLS material.
        assert!(!dir.path().join("tls_cert.pem").exists());
    }

    #[cfg(unix)]
    #[test]
    fn key_file_is_0600() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        load_or_generate(dir.path()).unwrap();
        let mode = std::fs::metadata(key_path(dir.path()))
            .unwrap()
            .permissions()
            .mode();
        assert_eq!(mode & 0o777, 0o600);
    }
}
