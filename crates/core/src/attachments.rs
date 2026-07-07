//! Discord (or other gateway) message attachments: capped, filtered download
//! into `.harness-attachments/{session_pk}`, plus the manifest text appended
//! to the agent prompt. Skip-reason strings, manifest strings, and check
//! order are exact contracts covered by this module's tests.
//!
//! ControlPlane wiring (dest dir under the session's attachment root, config
//! from settings) is a separate task — this module is the pure primitive.

use anyhow::Context;
use std::collections::HashSet;
use std::io::Read;
use std::path::PathBuf;
use std::sync::Arc;

pub use crate::domain::AttachmentRef;

/// Inputs to [`materialize_attachments`]: where to write accepted files and
/// the caps/allowlists that gate them.
#[derive(Debug, Clone)]
pub struct MaterializeOpts {
    pub dest_dir: PathBuf,
    pub max_bytes: u64,
    pub max_count: u32,
    /// Lowercase, no leading dot. Empty = allow all extensions.
    pub allowed_ext: Vec<String>,
    /// Lowercase hostnames. Empty = no host restriction.
    pub allowed_hosts: Vec<String>,
}

/// A file that was downloaded and written to `dest_dir`.
#[derive(Debug, Clone, PartialEq)]
pub struct SavedAttachment {
    pub path: PathBuf,
    pub name: String,
    pub content_type: Option<String>,
    pub size: u64,
}

/// A ref that was rejected before or during download, with a human-readable
/// reason (also used verbatim in [`build_manifest`]'s skip lines).
#[derive(Debug, Clone, PartialEq)]
pub struct SkippedAttachment {
    pub name: String,
    pub reason: String,
}

/// Outcome of one call to [`materialize_attachments`].
#[derive(Debug, Clone, PartialEq, Default)]
pub struct MaterializeResult {
    pub saved: Vec<SavedAttachment>,
    pub skipped: Vec<SkippedAttachment>,
}

/// Outcome of a single capped fetch. `TooBig` covers both a declared
/// `Content-Length` over `max_bytes` and the body itself streaming past the
/// cap; the fetcher decides which without ever buffering more than
/// `max_bytes` (+ a one-byte overrun to detect it) into memory.
#[derive(Debug)]
pub enum FetchOutcome {
    Ok(Vec<u8>),
    TooBig,
    HttpError(u16),
}

/// Downloads a single URL, capped at `max_bytes`. Implementations must never
/// buffer materially more than `max_bytes` bytes before giving up.
pub trait AttachmentFetcher: Send + Sync {
    fn fetch_capped(&self, url: &str, max_bytes: u64) -> anyhow::Result<FetchOutcome>;
}

/// Real network fetcher backed by `ureq`'s blocking client. Non-2xx
/// responses surface as `FetchOutcome::HttpError`; a declared
/// `Content-Length` over the cap short-circuits before any read; otherwise
/// the body is read through `Read::take(max_bytes + 1)` so a body that
/// exceeds the cap is detected without unbounded buffering.
pub struct UreqFetcher;

impl AttachmentFetcher for UreqFetcher {
    fn fetch_capped(&self, url: &str, max_bytes: u64) -> anyhow::Result<FetchOutcome> {
        if url.starts_with("file://") {
            let parsed = url::Url::parse(url).context("parse file attachment URL")?;
            let path = parsed
                .to_file_path()
                .map_err(|_| anyhow::anyhow!("invalid file attachment URL"))?;
            if std::fs::metadata(&path)?.len() > max_bytes {
                return Ok(FetchOutcome::TooBig);
            }
            let mut buf = Vec::new();
            std::fs::File::open(&path)?
                .take(max_bytes.saturating_add(1))
                .read_to_end(&mut buf)?;
            if buf.len() as u64 > max_bytes {
                return Ok(FetchOutcome::TooBig);
            }
            return Ok(FetchOutcome::Ok(buf));
        }

        let resp = match ureq::get(url).call() {
            Ok(resp) => resp,
            Err(ureq::Error::Status(code, _)) => return Ok(FetchOutcome::HttpError(code)),
            Err(e) => return Err(e.into()),
        };
        if let Some(declared) = resp
            .header("Content-Length")
            .and_then(|v| v.parse::<u64>().ok())
        {
            if declared > max_bytes {
                return Ok(FetchOutcome::TooBig);
            }
        }
        let mut buf = Vec::new();
        resp.into_reader()
            .take(max_bytes.saturating_add(1))
            .read_to_end(&mut buf)?;
        if buf.len() as u64 > max_bytes {
            return Ok(FetchOutcome::TooBig);
        }
        Ok(FetchOutcome::Ok(buf))
    }
}

struct Signature {
    exts: &'static [&'static str],
    magic: &'static [u8],
}

const SIGNATURES: &[Signature] = &[
    Signature {
        exts: &["png"],
        magic: &[0x89, 0x50, 0x4e, 0x47],
    },
    Signature {
        exts: &["jpg", "jpeg"],
        magic: &[0xff, 0xd8, 0xff],
    },
    Signature {
        exts: &["gif"],
        magic: &[0x47, 0x49, 0x46, 0x38],
    },
    Signature {
        exts: &["pdf"],
        magic: &[0x25, 0x50, 0x44, 0x46],
    },
    Signature {
        exts: &["zip"],
        magic: &[0x50, 0x4b, 0x03, 0x04],
    },
    Signature {
        exts: &["gz", "gzip"],
        magic: &[0x1f, 0x8b],
    },
    Signature {
        exts: &["exe", "dll"],
        magic: &[0x4d, 0x5a],
    },
    Signature {
        exts: &["elf"],
        magic: &[0x7f, 0x45, 0x4c, 0x46],
    },
];

/// Lowercase extension without the dot, or `""` if `name` has none.
fn ext_of(name: &str) -> String {
    let lower = name.to_lowercase();
    match lower.rfind('.') {
        Some(idx) => {
            let candidate = &lower[idx + 1..];
            if !candidate.is_empty()
                && candidate
                    .chars()
                    .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit())
            {
                candidate.to_string()
            } else {
                String::new()
            }
        }
        None => String::new(),
    }
}

/// Contradiction-only anti-spoof check: if `ext` implies a known magic
/// signature, `bytes` must start with it. Unknown/text extensions never
/// trip this (they have no signature to contradict).
fn contradicts_extension(ext: &str, bytes: &[u8]) -> bool {
    match SIGNATURES.iter().find(|s| s.exts.contains(&ext)) {
        None => false,
        Some(sig) => !bytes.starts_with(sig.magic),
    }
}

/// Strips path components (both slash styles), replaces any character
/// outside `[A-Za-z0-9._-]` with `_`, strips leading dots, and falls back to
/// `"file"` if nothing survives.
pub fn sanitize_name(name: &str) -> String {
    let base = name.rsplit(['/', '\\']).next().unwrap_or("");
    let cleaned: String = base
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '.' || c == '_' || c == '-' {
                c
            } else {
                '_'
            }
        })
        .collect();
    let cleaned = cleaned.trim_start_matches('.');
    if cleaned.is_empty() {
        "file".to_string()
    } else {
        cleaned.to_string()
    }
}

/// Appends `-1`, `-2`, ... before the last extension until `name` is unique
/// within `used`, then reserves the chosen name.
fn dedupe(name: String, used: &mut HashSet<String>) -> String {
    if !used.contains(&name) {
        used.insert(name.clone());
        return name;
    }
    let dot = name.rfind('.');
    let (stem, ext) = match dot {
        Some(idx) if idx > 0 => (name[..idx].to_string(), name[idx..].to_string()),
        _ => (name.clone(), String::new()),
    };
    let mut i: u32 = 1;
    loop {
        let candidate = format!("{stem}-{i}{ext}");
        if !used.contains(&candidate) {
            used.insert(candidate.clone());
            return candidate;
        }
        i += 1;
    }
}

/// Parses a comma-separated extension allowlist: trims, lowercases, strips
/// one leading dot, drops empties. `None`/empty input means "allow all".
pub fn parse_allowed_ext(raw: Option<&str>) -> Vec<String> {
    raw.unwrap_or("")
        .split(',')
        .filter_map(|s| {
            let s = s.trim().to_lowercase();
            let s = s.strip_prefix('.').unwrap_or(&s).to_string();
            if s.is_empty() {
                None
            } else {
                Some(s)
            }
        })
        .collect()
}

/// Parses a comma-separated host allowlist: trims, lowercases, drops
/// empties. `None`/empty input means "no host restriction".
pub fn parse_allowed_hosts(raw: Option<&str>) -> Vec<String> {
    raw.unwrap_or("")
        .split(',')
        .filter_map(|s| {
            let s = s.trim().to_lowercase();
            if s.is_empty() {
                None
            } else {
                Some(s)
            }
        })
        .collect()
}

/// `https://` scheme required; returns the lowercase hostname, or `None` for
/// non-https or unparsable URLs (both treated as "untrusted").
fn https_host(raw: &str) -> Option<String> {
    let parsed = url::Url::parse(raw).ok()?;
    if parsed.scheme() != "https" {
        return None;
    }
    parsed.host_str().map(|h| h.to_lowercase())
}

fn format_bytes(n: u64) -> String {
    if n < 1024 {
        return format!("{n} B");
    }
    if n < 1024 * 1024 {
        return format!("{} KB", ((n as f64) / 1024.0).round() as u64);
    }
    format!("{:.1} MB", (n as f64) / (1024.0 * 1024.0))
}

/// Control characters (`< 0x20`) become spaces; capped at 120 characters —
/// only used for the manifest's skip lines, never for the on-disk filename.
fn display_name(name: &str) -> String {
    name.chars()
        .take(120)
        .map(|c| if (c as u32) < 32 { ' ' } else { c })
        .collect()
}

/// Downloads and writes each accepted ref under `opts.dest_dir`, in order,
/// per-ref check order: accepted-count cap → declared size → extension
/// allowlist → host allowlist → fetch (HTTP error / declared or streamed
/// size cap) → magic-byte contradiction → sanitize + dedupe + write.
///
/// The fetch itself runs via [`tokio::task::spawn_blocking`] (it's
/// `ureq`'s blocking client under the hood) so it never stalls the async
/// executor; `fetcher` is an `Arc` rather than the brief-sketched `&dyn`
/// reference specifically so it can be cloned into that spawned task — a
/// borrowed trait object can't cross into a `'static`-bound spawned task
/// without either unsound lifetime-laundering or `block_in_place` (which
/// panics on the `current_thread` runtime flavor this crate's `#[tokio::test]`s
/// use by default). See the task report for the full rationale.
pub async fn materialize_attachments(
    refs: &[AttachmentRef],
    opts: &MaterializeOpts,
    fetcher: Arc<dyn AttachmentFetcher>,
) -> anyhow::Result<MaterializeResult> {
    let mut saved = Vec::new();
    let mut skipped = Vec::new();
    let mut used: HashSet<String> = HashSet::new();
    let mut accepted: u32 = 0;

    for r in refs {
        if accepted >= opts.max_count {
            skipped.push(SkippedAttachment {
                name: r.name.clone(),
                reason: "too many attachments".to_string(),
            });
            continue;
        }
        if r.size > opts.max_bytes {
            skipped.push(SkippedAttachment {
                name: r.name.clone(),
                reason: format!("exceeds {} bytes", opts.max_bytes),
            });
            continue;
        }

        let ext = ext_of(&r.name);
        if !opts.allowed_ext.is_empty() && !opts.allowed_ext.contains(&ext) {
            skipped.push(SkippedAttachment {
                name: r.name.clone(),
                reason: "extension not allowed".to_string(),
            });
            continue;
        }

        if !opts.allowed_hosts.is_empty() && !r.url.starts_with("file://") {
            let trusted = https_host(&r.url).is_some_and(|h| opts.allowed_hosts.contains(&h));
            if !trusted {
                skipped.push(SkippedAttachment {
                    name: r.name.clone(),
                    reason: "untrusted host".to_string(),
                });
                continue;
            }
        }

        let url = r.url.clone();
        let max_bytes = opts.max_bytes;
        let fetch_task_fetcher = Arc::clone(&fetcher);
        let outcome = match tokio::task::spawn_blocking(move || {
            fetch_task_fetcher.fetch_capped(&url, max_bytes)
        })
        .await
        {
            Ok(inner) => inner,
            Err(join_err) => {
                skipped.push(SkippedAttachment {
                    name: r.name.clone(),
                    reason: format!("download failed: {join_err}"),
                });
                continue;
            }
        };

        let bytes = match outcome {
            Ok(FetchOutcome::Ok(bytes)) => {
                if bytes.len() as u64 > opts.max_bytes {
                    skipped.push(SkippedAttachment {
                        name: r.name.clone(),
                        reason: format!("exceeds {} bytes", opts.max_bytes),
                    });
                    continue;
                }
                bytes
            }
            Ok(FetchOutcome::TooBig) => {
                skipped.push(SkippedAttachment {
                    name: r.name.clone(),
                    reason: format!("exceeds {} bytes", opts.max_bytes),
                });
                continue;
            }
            Ok(FetchOutcome::HttpError(status)) => {
                skipped.push(SkippedAttachment {
                    name: r.name.clone(),
                    reason: format!("download failed: HTTP {status}"),
                });
                continue;
            }
            Err(e) => {
                skipped.push(SkippedAttachment {
                    name: r.name.clone(),
                    reason: format!("download failed: {e}"),
                });
                continue;
            }
        };

        if contradicts_extension(&ext, &bytes) {
            skipped.push(SkippedAttachment {
                name: r.name.clone(),
                reason: "content does not match extension".to_string(),
            });
            continue;
        }

        std::fs::create_dir_all(&opts.dest_dir)
            .with_context(|| format!("create attachment dest dir {}", opts.dest_dir.display()))?;
        let file_name = dedupe(sanitize_name(&r.name), &mut used);
        let path = opts.dest_dir.join(&file_name);
        std::fs::write(&path, &bytes)
            .with_context(|| format!("write attachment to disk at {}", path.display()))?;
        saved.push(SavedAttachment {
            path,
            name: r.name.clone(),
            content_type: r.content_type.clone(),
            size: bytes.len() as u64,
        });
        accepted += 1;
    }

    Ok(MaterializeResult { saved, skipped })
}

/// Renders the manifest text appended to the agent prompt: a header + one
/// line per saved file when anything was saved, then one `⚠️ Skipped ...`
/// line per rejected ref. Empty input (nothing saved, nothing skipped)
/// renders as `""`.
pub fn build_manifest(result: &MaterializeResult) -> String {
    let mut lines: Vec<String> = Vec::new();
    if !result.saved.is_empty() {
        let n = result.saved.len();
        let plural = if n > 1 { "s" } else { "" };
        lines.push(format!(
            "[User attached {n} file{plural} — saved to disk, use the Read tool to open them:]"
        ));
        for f in &result.saved {
            let content_type = f.content_type.as_deref().unwrap_or("unknown");
            lines.push(format!(
                "- {} ({content_type}, {})",
                f.path.display(),
                format_bytes(f.size)
            ));
        }
    }
    for s in &result.skipped {
        lines.push(format!(
            "⚠️ Skipped {}: {}",
            display_name(&s.name),
            s.reason
        ));
    }
    lines.join("\n")
}

/// Images at or under this size become inline vision blocks; larger ones stay
/// manifest-only (the model can still Read them from disk).
pub const IMAGE_BLOCK_MAX_BYTES: u64 = 5 * 1024 * 1024;
/// At most this many images become vision blocks per message.
pub const IMAGE_BLOCK_MAX_COUNT: usize = 10;

/// Anthropic-supported raster media types (vision block eligibility).
pub fn image_block_media_type(content_type: Option<&str>) -> Option<&'static str> {
    match content_type? {
        "image/png" => Some("image/png"),
        "image/jpeg" => Some("image/jpeg"),
        "image/gif" => Some("image/gif"),
        "image/webp" => Some("image/webp"),
        _ => None,
    }
}

/// Build Anthropic `{type:"image", source:{type:"base64",…}}` blocks for the
/// eligible saved images (supported media type, within the size cap), up to
/// the count cap. Files that fail to read are skipped — the manifest still
/// lists every saved file either way.
pub async fn image_blocks_for(saved: &[SavedAttachment]) -> Vec<serde_json::Value> {
    use base64::Engine as _;
    let mut blocks = Vec::new();
    for f in saved {
        if blocks.len() >= IMAGE_BLOCK_MAX_COUNT {
            break;
        }
        let Some(media_type) = image_block_media_type(f.content_type.as_deref()) else {
            continue;
        };
        if f.size > IMAGE_BLOCK_MAX_BYTES {
            continue;
        }
        let Ok(bytes) = tokio::fs::read(&f.path).await else {
            continue;
        };
        let data = base64::engine::general_purpose::STANDARD.encode(bytes);
        blocks.push(serde_json::json!({
            "type": "image",
            "source": { "type": "base64", "media_type": media_type, "data": data }
        }));
    }
    blocks
}

/// Display metadata persisted on the user transcript row so the cockpit can
/// re-render previews after reload: `{name, path, contentType, size}`.
pub fn attachment_display_meta(saved: &[SavedAttachment]) -> Vec<serde_json::Value> {
    saved
        .iter()
        .map(|f| {
            serde_json::json!({
                "name": f.name,
                "path": f.path.to_string_lossy(),
                "contentType": f.content_type,
                "size": f.size,
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;

    const PNG: &[u8] = &[0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a];
    const ELF: &[u8] = &[0x7f, 0x45, 0x4c, 0x46, 1, 1, 1, 0];
    const TXT: &[u8] = b"just a log line\n";

    #[derive(Clone)]
    struct FakeBody {
        bytes: Vec<u8>,
        declared_len: Option<u64>,
    }

    struct FakeFetcher {
        bodies: HashMap<String, FakeBody>,
        calls: Arc<AtomicUsize>,
    }

    impl FakeFetcher {
        fn new(bodies: impl IntoIterator<Item = (&'static str, &'static [u8])>) -> Self {
            Self {
                bodies: bodies
                    .into_iter()
                    .map(|(k, v)| {
                        (
                            k.to_string(),
                            FakeBody {
                                bytes: v.to_vec(),
                                declared_len: None,
                            },
                        )
                    })
                    .collect(),
                calls: Arc::new(AtomicUsize::new(0)),
            }
        }

        fn with_declared_len(mut self, url: &str, len: u64) -> Self {
            if let Some(b) = self.bodies.get_mut(url) {
                b.declared_len = Some(len);
            }
            self
        }
    }

    impl AttachmentFetcher for FakeFetcher {
        fn fetch_capped(&self, url: &str, max_bytes: u64) -> anyhow::Result<FetchOutcome> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            let Some(body) = self.bodies.get(url) else {
                return Ok(FetchOutcome::HttpError(404));
            };
            if let Some(declared) = body.declared_len {
                if declared > max_bytes {
                    return Ok(FetchOutcome::TooBig);
                }
            }
            if body.bytes.len() as u64 > max_bytes {
                return Ok(FetchOutcome::TooBig);
            }
            Ok(FetchOutcome::Ok(body.bytes.clone()))
        }
    }

    fn base_ref() -> AttachmentRef {
        AttachmentRef {
            name: "a.png".into(),
            url: "https://cdn/a".into(),
            content_type: Some("image/png".into()),
            size: PNG.len() as u64,
        }
    }

    fn base_opts(dir: &std::path::Path) -> MaterializeOpts {
        MaterializeOpts {
            dest_dir: dir.to_path_buf(),
            max_bytes: 1_000_000,
            max_count: 10,
            allowed_ext: vec![],
            allowed_hosts: vec![],
        }
    }

    #[tokio::test]
    async fn saves_a_valid_file_and_reports_it() {
        let dir = tempfile::tempdir().unwrap();
        let fetcher = Arc::new(FakeFetcher::new([("https://cdn/a", PNG)]));
        let res = materialize_attachments(&[base_ref()], &base_opts(dir.path()), fetcher)
            .await
            .unwrap();
        assert_eq!(res.saved.len(), 1);
        assert_eq!(res.skipped.len(), 0);
        assert!(res.saved[0].path.exists());
        assert_eq!(res.saved[0].size, PNG.len() as u64);
    }

    #[tokio::test]
    async fn skips_oversize_before_downloading() {
        let dir = tempfile::tempdir().unwrap();
        let fetcher = Arc::new(FakeFetcher::new([("https://cdn/a", PNG)]));
        let calls = Arc::clone(&fetcher.calls);
        let r = AttachmentRef {
            size: 999_999,
            ..base_ref()
        };
        let opts = MaterializeOpts {
            max_bytes: 10,
            ..base_opts(dir.path())
        };
        let res = materialize_attachments(&[r], &opts, fetcher).await.unwrap();
        assert_eq!(res.saved.len(), 0);
        assert!(res.skipped[0].reason.contains("exceeds"));
        assert_eq!(calls.load(Ordering::SeqCst), 0, "never fetched");
    }

    #[tokio::test]
    async fn enforces_max_count_extras_skipped_as_too_many() {
        let dir = tempfile::tempdir().unwrap();
        let fetcher = Arc::new(FakeFetcher::new([("u1", PNG), ("u2", PNG), ("u3", PNG)]));
        let refs = vec![
            AttachmentRef {
                url: "u1".into(),
                ..base_ref()
            },
            AttachmentRef {
                url: "u2".into(),
                ..base_ref()
            },
            AttachmentRef {
                url: "u3".into(),
                ..base_ref()
            },
        ];
        let opts = MaterializeOpts {
            max_count: 2,
            ..base_opts(dir.path())
        };
        let res = materialize_attachments(&refs, &opts, fetcher)
            .await
            .unwrap();
        assert_eq!(res.saved.len(), 2);
        assert_eq!(
            res.skipped
                .iter()
                .filter(|s| s.reason.contains("too many"))
                .count(),
            1
        );
    }

    #[tokio::test]
    async fn extension_allowlist_filters_by_extension() {
        let dir = tempfile::tempdir().unwrap();
        let fetcher = Arc::new(FakeFetcher::new([("u1", TXT), ("u2", PNG)]));
        let refs = vec![
            AttachmentRef {
                name: "doc.txt".into(),
                url: "u1".into(),
                ..base_ref()
            },
            AttachmentRef {
                name: "img.png".into(),
                url: "u2".into(),
                ..base_ref()
            },
        ];
        let opts = MaterializeOpts {
            allowed_ext: vec!["png".into()],
            ..base_opts(dir.path())
        };
        let res = materialize_attachments(&refs, &opts, fetcher)
            .await
            .unwrap();
        assert_eq!(
            res.saved.iter().map(|s| s.name.clone()).collect::<Vec<_>>(),
            vec!["img.png".to_string()]
        );
        assert!(res.skipped[0].reason.contains("extension not allowed"));
    }

    #[tokio::test]
    async fn rejects_content_that_contradicts_its_extension() {
        let dir = tempfile::tempdir().unwrap();
        let fetcher = Arc::new(FakeFetcher::new([("u1", ELF)]));
        let r = AttachmentRef {
            name: "evil.png".into(),
            url: "u1".into(),
            ..base_ref()
        };
        let res = materialize_attachments(&[r], &base_opts(dir.path()), fetcher)
            .await
            .unwrap();
        assert_eq!(res.saved.len(), 0);
        assert!(res.skipped[0].reason.contains("does not match extension"));
    }

    #[tokio::test]
    async fn text_unknown_extension_passes_the_mime_check() {
        let dir = tempfile::tempdir().unwrap();
        let fetcher = Arc::new(FakeFetcher::new([("u1", TXT)]));
        let r = AttachmentRef {
            name: "server.log".into(),
            url: "u1".into(),
            content_type: Some("text/plain".into()),
            ..base_ref()
        };
        let res = materialize_attachments(&[r], &base_opts(dir.path()), fetcher)
            .await
            .unwrap();
        assert_eq!(res.saved.len(), 1);
    }

    #[tokio::test]
    async fn download_failure_skips_the_file_others_continue() {
        let dir = tempfile::tempdir().unwrap();
        let fetcher = Arc::new(FakeFetcher::new([("u2", PNG)]));
        let refs = vec![
            AttachmentRef {
                name: "gone.png".into(),
                url: "missing".into(),
                ..base_ref()
            },
            AttachmentRef {
                name: "ok.png".into(),
                url: "u2".into(),
                ..base_ref()
            },
        ];
        let res = materialize_attachments(&refs, &base_opts(dir.path()), fetcher)
            .await
            .unwrap();
        assert_eq!(
            res.saved.iter().map(|s| s.name.clone()).collect::<Vec<_>>(),
            vec!["ok.png".to_string()]
        );
        assert!(res.skipped[0].reason.contains("download failed"));
    }

    #[tokio::test]
    async fn sanitizes_traversal_names_and_dedupes_collisions() {
        let dir = tempfile::tempdir().unwrap();
        let fetcher = Arc::new(FakeFetcher::new([("u1", PNG), ("u2", PNG), ("u3", TXT)]));
        let refs = vec![
            AttachmentRef {
                name: "shot.png".into(),
                url: "u1".into(),
                ..base_ref()
            },
            AttachmentRef {
                name: "shot.png".into(),
                url: "u2".into(),
                ..base_ref()
            },
            AttachmentRef {
                name: "../../etc/passwd".into(),
                url: "u3".into(),
                content_type: Some("text/plain".into()),
                ..base_ref()
            },
        ];
        let res = materialize_attachments(&refs, &base_opts(dir.path()), fetcher)
            .await
            .unwrap();
        let bases: Vec<String> = res
            .saved
            .iter()
            .map(|s| {
                s.path
                    .strip_prefix(dir.path())
                    .unwrap()
                    .to_string_lossy()
                    .into_owned()
            })
            .collect();
        assert!(bases.contains(&"shot.png".to_string()));
        assert!(bases.contains(&"shot-1.png".to_string()));
        assert!(bases.contains(&"passwd".to_string()));
        assert!(bases.iter().all(|b| !b.contains('/') && !b.contains("..")));
    }

    #[test]
    fn build_manifest_lists_saved_paths_and_skips_empty_when_nothing() {
        assert_eq!(
            build_manifest(&MaterializeResult {
                saved: vec![],
                skipped: vec![]
            }),
            ""
        );
        let text = build_manifest(&MaterializeResult {
            saved: vec![SavedAttachment {
                path: "/x/a.png".into(),
                name: "a.png".into(),
                content_type: Some("image/png".into()),
                size: 240_000,
            }],
            skipped: vec![SkippedAttachment {
                name: "huge.zip".into(),
                reason: "exceeds 26214400 bytes".into(),
            }],
        });
        assert!(text.contains("/x/a.png"));
        assert!(text.contains("image/png"));
        assert!(text.contains("Skipped huge.zip"));
        assert!(text.contains("exceeds"));
    }

    #[test]
    fn ureq_fetcher_reads_capped_file_urls_for_cockpit_attachments() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("note.txt");
        std::fs::write(&path, b"hello from disk").unwrap();
        let url = url::Url::from_file_path(&path).unwrap().to_string();

        match UreqFetcher.fetch_capped(&url, 1_000).unwrap() {
            FetchOutcome::Ok(bytes) => assert_eq!(bytes, b"hello from disk"),
            other => panic!("expected file bytes, got {other:?}"),
        }

        assert!(matches!(
            UreqFetcher.fetch_capped(&url, 4).unwrap(),
            FetchOutcome::TooBig
        ));
    }

    #[tokio::test]
    async fn rejects_a_body_larger_than_max_bytes_via_content_length_before_buffering() {
        let dir = tempfile::tempdir().unwrap();
        let fetcher = Arc::new(FakeFetcher::new([("u1", PNG)]).with_declared_len("u1", 500));
        let calls = Arc::clone(&fetcher.calls);
        let r = AttachmentRef {
            name: "a.png".into(),
            url: "u1".into(),
            size: 10,
            ..base_ref()
        };
        let opts = MaterializeOpts {
            max_bytes: 100,
            ..base_opts(dir.path())
        };
        let res = materialize_attachments(&[r], &opts, fetcher).await.unwrap();
        assert_eq!(res.saved.len(), 0);
        assert!(res.skipped[0].reason.contains("exceeds"));
        assert_eq!(calls.load(Ordering::SeqCst), 1, "fetch happened");
    }

    #[test]
    fn parse_allowed_ext_normalizes_and_sanitize_name_strips_paths() {
        assert_eq!(
            parse_allowed_ext(Some("PNG, .jpg ,,")),
            vec!["png".to_string(), "jpg".to_string()]
        );
        assert_eq!(parse_allowed_ext(None), Vec::<String>::new());
        assert_eq!(
            parse_allowed_hosts(Some("cdn.discordapp.com, Media.DiscordApp.net ,,")),
            vec![
                "cdn.discordapp.com".to_string(),
                "media.discordapp.net".to_string()
            ]
        );
        assert_eq!(parse_allowed_hosts(None), Vec::<String>::new());
        assert_eq!(sanitize_name("../x.png"), "x.png");
        assert_eq!(sanitize_name("a b.png"), "a_b.png");
        assert_eq!(sanitize_name(".."), "file");
    }

    #[tokio::test]
    async fn host_allowlist_allows_trusted_https_blocks_untrusted_and_http() {
        let dir = tempfile::tempdir().unwrap();
        let fetcher = Arc::new(FakeFetcher::new([
            ("https://cdn.discordapp.com/x", PNG),
            ("https://evil.com/x", PNG),
            ("http://cdn.discordapp.com/x", PNG),
        ]));
        let refs = vec![
            AttachmentRef {
                name: "a.png".into(),
                url: "https://cdn.discordapp.com/x".into(),
                ..base_ref()
            },
            AttachmentRef {
                name: "b.png".into(),
                url: "https://evil.com/x".into(),
                ..base_ref()
            },
            AttachmentRef {
                name: "c.png".into(),
                url: "http://cdn.discordapp.com/x".into(),
                ..base_ref()
            },
        ];
        let opts = MaterializeOpts {
            allowed_hosts: vec!["cdn.discordapp.com".into()],
            ..base_opts(dir.path())
        };
        let res = materialize_attachments(&refs, &opts, fetcher)
            .await
            .unwrap();
        assert_eq!(res.saved.len(), 1);
        assert_eq!(res.saved[0].name, "a.png");
        assert_eq!(res.skipped.len(), 2);
        assert!(res
            .skipped
            .iter()
            .any(|s| s.name == "b.png" && s.reason.contains("untrusted host")));
        assert!(res
            .skipped
            .iter()
            .any(|s| s.name == "c.png" && s.reason.contains("untrusted host")));
    }

    #[tokio::test]
    async fn streaming_cap_fires_without_content_length_header() {
        let dir = tempfile::tempdir().unwrap();
        let big = vec![0u8; 500];
        let fetcher = Arc::new(FakeFetcher {
            bodies: HashMap::from([(
                "https://cdn/x".to_string(),
                FakeBody {
                    bytes: big,
                    declared_len: None,
                },
            )]),
            calls: Arc::new(AtomicUsize::new(0)),
        });
        let r = AttachmentRef {
            name: "big.png".into(),
            url: "https://cdn/x".into(),
            size: 10,
            ..base_ref()
        };
        let opts = MaterializeOpts {
            max_bytes: 100,
            ..base_opts(dir.path())
        };
        let res = materialize_attachments(&[r], &opts, fetcher).await.unwrap();
        assert_eq!(res.saved.len(), 0);
        assert!(res.skipped[0].reason.contains("exceeds"));
    }

    #[test]
    fn image_block_media_types_cover_the_anthropic_raster_set() {
        assert_eq!(image_block_media_type(Some("image/png")), Some("image/png"));
        assert_eq!(image_block_media_type(Some("image/jpeg")), Some("image/jpeg"));
        assert_eq!(image_block_media_type(Some("image/gif")), Some("image/gif"));
        assert_eq!(image_block_media_type(Some("image/webp")), Some("image/webp"));
        assert_eq!(image_block_media_type(Some("application/pdf")), None);
        assert_eq!(image_block_media_type(None), None);
    }

    #[tokio::test]
    async fn image_blocks_encode_eligible_files_and_skip_oversized_and_nonimage() {
        let dir = std::env::temp_dir().join(format!("ryuzi-imgblk-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let png = dir.join("a.png");
        std::fs::write(&png, [0x89, 0x50, 0x4E, 0x47]).unwrap();
        let saved = vec![
            SavedAttachment { path: png.clone(), name: "a.png".into(), content_type: Some("image/png".into()), size: 4 },
            SavedAttachment { path: dir.join("b.mp4"), name: "b.mp4".into(), content_type: Some("video/mp4".into()), size: 4 },
            SavedAttachment { path: dir.join("huge.png"), name: "huge.png".into(), content_type: Some("image/png".into()), size: IMAGE_BLOCK_MAX_BYTES + 1 },
        ];
        let blocks = image_blocks_for(&saved).await;
        assert_eq!(blocks.len(), 1);
        assert_eq!(blocks[0]["type"], "image");
        assert_eq!(blocks[0]["source"]["type"], "base64");
        assert_eq!(blocks[0]["source"]["media_type"], "image/png");
        assert_eq!(blocks[0]["source"]["data"], "iVBORw==");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn attachment_display_meta_maps_saved_files() {
        let saved = vec![SavedAttachment {
            path: PathBuf::from("/tmp/x/a.png"),
            name: "a.png".into(),
            content_type: Some("image/png".into()),
            size: 12,
        }];
        let meta = attachment_display_meta(&saved);
        assert_eq!(meta.len(), 1);
        assert_eq!(meta[0]["name"], "a.png");
        assert_eq!(meta[0]["contentType"], "image/png");
        assert_eq!(meta[0]["size"], 12);
        assert!(meta[0]["path"].as_str().unwrap().ends_with("a.png"));
    }
}
