//! GitHub release check with injectable HTTP layer.
//! Never throws: any network/HTTP/JSON failure yields a `none` result so a
//! periodic check can never crash the daemon.

use super::version::is_newer;

/// HTTP response body and status.
#[derive(Debug, Clone, PartialEq)]
pub struct HttpResponse {
    pub status: u16,
    pub body: Vec<u8>,
}

/// Abstraction over HTTP GET for dependency injection (enables testing + multiple impls).
pub trait UpdateHttp: Send + Sync {
    /// GET request to `url`. Non-2xx is a response (not an error).
    fn get(&self, url: &str) -> anyhow::Result<HttpResponse>;
}

/// Production HTTP implementation using ureq (blocks — use with spawn_blocking in async contexts).
pub struct UreqHttp;

impl UpdateHttp for UreqHttp {
    fn get(&self, url: &str) -> anyhow::Result<HttpResponse> {
        use anyhow::Context;
        use std::io::Read;

        let result = ureq::get(url)
            .set("Accept", "application/vnd.github+json")
            .set("User-Agent", "ryuzi")
            .call();
        let (status, resp) = match result {
            Ok(r) => (r.status(), r),
            // Non-2xx is a RESPONSE (callers branch on status), not an error.
            Err(ureq::Error::Status(code, r)) => (code, r),
            Err(e) => return Err(e).with_context(|| format!("GET {url}")),
        };
        let mut body = Vec::new();
        resp.into_reader().read_to_end(&mut body)?;
        Ok(HttpResponse { status, body })
    }
}

/// Check result with current, latest, and whether an update is available.
#[derive(Debug, Clone, PartialEq)]
pub struct UpdateCheckResult {
    pub current_version: String,
    pub latest_version: Option<String>,
    pub update_available: bool,
    pub tag: Option<String>,
}

impl UpdateCheckResult {
    /// Construct a "no update available" result for the given current version.
    pub fn none(current: &str) -> Self {
        Self {
            current_version: current.to_string(),
            latest_version: None,
            update_available: false,
            tag: None,
        }
    }
}

/// Check GitHub Releases for an update. Never throws: any network error,
/// non-OK status, or missing `tag_name` yields `none`.
pub fn check_for_update(
    current_version: &str,
    repo: &str,
    http: &dyn UpdateHttp,
) -> UpdateCheckResult {
    let none = UpdateCheckResult::none(current_version);
    let url = format!("https://api.github.com/repos/{repo}/releases/latest");
    let Ok(resp) = http.get(&url) else {
        return none;
    };
    if !(200..300).contains(&resp.status) {
        return none;
    }
    let Ok(body) = serde_json::from_slice::<serde_json::Value>(&resp.body) else {
        return none;
    };
    let Some(tag) = body.get("tag_name").and_then(|v| v.as_str()) else {
        return none;
    };
    let latest = tag.strip_prefix(['v', 'V']).unwrap_or(tag).to_string();
    UpdateCheckResult {
        current_version: current_version.to_string(),
        update_available: is_newer(current_version, &latest),
        latest_version: Some(latest),
        tag: Some(tag.to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    struct FakeHttp {
        status: u16,
        body: &'static str,
        err: bool,
        seen: Mutex<Vec<String>>,
    }

    impl UpdateHttp for FakeHttp {
        fn get(&self, url: &str) -> anyhow::Result<HttpResponse> {
            self.seen.lock().unwrap().push(url.to_string());
            if self.err {
                anyhow::bail!("offline")
            }
            Ok(HttpResponse {
                status: self.status,
                body: self.body.as_bytes().to_vec(),
            })
        }
    }

    fn http(status: u16, body: &'static str) -> FakeHttp {
        FakeHttp {
            status,
            body,
            err: false,
            seen: Mutex::new(vec![]),
        }
    }

    #[test]
    fn newer_tag_reports_update_and_strips_v() {
        let h = http(200, r#"{"tag_name":"v0.3.0"}"#);
        let r = check_for_update("0.2.0", "o/r", &h);
        assert!(r.update_available);
        assert_eq!(r.latest_version.as_deref(), Some("0.3.0"));
        assert_eq!(r.tag.as_deref(), Some("v0.3.0"));
        assert_eq!(
            h.seen.lock().unwrap()[0],
            "https://api.github.com/repos/o/r/releases/latest"
        );
    }

    #[test]
    fn same_or_older_tag_reports_no_update() {
        let r = check_for_update("0.3.0", "o/r", &http(200, r#"{"tag_name":"v0.3.0"}"#));
        assert!(!r.update_available);
        assert_eq!(r.latest_version.as_deref(), Some("0.3.0")); // still reported
    }

    #[test]
    fn non_ok_status_missing_tag_bad_json_and_errors_yield_none() {
        for h in [
            http(404, "{}"),
            http(200, "{}"),
            http(200, "not json"),
            FakeHttp {
                status: 200,
                body: "",
                err: true,
                seen: Mutex::new(vec![]),
            },
        ] {
            let r = check_for_update("0.2.0", "o/r", &h);
            assert_eq!(
                r,
                UpdateCheckResult::none("0.2.0"),
                "must never claim an update"
            );
        }
    }
}
