//! Pure, host-free Bitbucket connector logic. Every function here is
//! deterministic over its inputs (no network, no clock, no storage), so the
//! whole module is covered by native `cargo test`. The wasm `guest` glue
//! supplies the live effect — one `oauth.authorized-request("bitbucket-cloud",
//! ..)` per planned request — and maps these plain types to/from WIT.
//!
//! # Endpoints (verified against the live Bitbucket Cloud OpenAPI document at
//! `https://api.bitbucket.org/swagger.json`, and developer.atlassian.com/cloud/bitbucket,
//! July 2026)
//! As of May 4 2026 every OAuth-authenticated Bitbucket Cloud REST call is
//! directed at `https://api.bitbucket.org/2.0/...` (the legacy scheme of
//! per-workspace hosts is gone). Path templates confirmed against the
//! `paths` map of the live OpenAPI document:
//!   * `GET /2.0/user` — the authenticated user (`auth_status`).
//!   * `GET /2.0/repositories/{workspace}` — `repo_list` (optional `role`,
//!     `q` query params).
//!   * `GET /2.0/repositories/{workspace}/{repo_slug}` — `repo_get`.
//!   * `GET /2.0/repositories/{workspace}/{repo_slug}/pullrequests` —
//!     `pr_list` (optional `state` query param).
//!   * `POST /2.0/repositories/{workspace}/{repo_slug}/pullrequests` —
//!     `pr_create`. Body: `title` (string), `source.branch.name` (string),
//!     `destination.branch.name` (string), optional `description` (string,
//!     plain — NOT the nested `summary.raw` shape the read model returns
//!     it as) and `close_source_branch` (bool).
//!   * `POST /2.0/repositories/{workspace}/{repo_slug}/pullrequests/{pull_request_id}/merge`
//!     — `pr_merge`. Body: optional `message`, `merge_strategy`
//!     (`merge_commit`|`squash`|`fast_forward`|`squash_fast_forward`|
//!     `rebase_fast_forward`|`rebase_merge`), `close_source_branch`. Returns
//!     `200` with the merged pull request synchronously, or `202` when
//!     Bitbucket queues the merge for asynchronous processing.
//!   * `POST /2.0/repositories/{workspace}/{repo_slug}/pullrequests/{pull_request_id}/comments`
//!     — `pr_comment`. Body: `content.raw` (string).
//!   * `GET /2.0/repositories/{workspace}/{repo_slug}/issues` —
//!     `issue_list` (no documented query filters, unlike `pullrequests`).
//!   * `POST /2.0/repositories/{workspace}/{repo_slug}/issues` —
//!     `issue_create`. Body: `title` (string), optional `kind`
//!     (`bug`|`enhancement`|`proposal`|`task`), `priority`
//!     (`trivial`|`minor`|`major`|`critical`|`blocker`), `content.raw`
//!     (string).
//!
//! Bitbucket's own error envelope (the `error` OpenAPI definition) is
//! `{"type":"error","error":{"message":"...","detail":"..."}}` — see
//! [`bitbucket_message`].
//!
//! # Why `workspace`/`repo_slug` are plain arguments
//! See the crate root doc: this bundle's `lifecycle` is `per-call`, so a
//! resolve-and-cache approach has nowhere to keep a cached value between one
//! `invoke` and the next. Every repo/PR/issue tool therefore takes explicit
//! `workspace`/`repo_slug` arguments (mirroring how the Atlassian connector
//! requires `cloud_id`).
//!
//! The two seams the guest drives:
//!   * [`plan_request`] turns a `(tool, args)` pair into a [`PlannedRequest`]
//!     (method/url/headers/body) — or an error. Crucially, a *mutating* tool
//!     invoked without `confirm=true` returns [`ToolError::ConfirmationRequired`]
//!     WITHOUT ever building a request, so an unconfirmed mutation is provably
//!     never sent.
//!   * [`parse_response`] turns a `(tool, status, body)` triple into the
//!     connector `tool-value` list the model sees — or an error.

use serde_json::{Map, Value};

/// Bitbucket Cloud's REST API 2.0 base. Every planned request targets this
/// host, which the manifest's `api.bitbucket.org` network entry authorizes.
/// As of May 2026 this is the ONLY host OAuth-authenticated Bitbucket API
/// traffic is directed at — there is no per-workspace host to fall back to.
pub const API_BASE: &str = "https://api.bitbucket.org/2.0";

/// The OAuth profile id the guest passes to `authorized-request` — matches
/// the `[[oauth]] id` in `ryuzi-plugin.toml`. DISTINCT from Atlassian's
/// `atlassian-cloud` profile (see the crate root doc) — this component must
/// never reference `"atlassian-cloud"`.
pub const OAUTH_PROFILE: &str = "bitbucket-cloud";

/// The `Accept` media type every Bitbucket Cloud REST call uses.
pub const ACCEPT: &str = "application/json";

/// Host-free mirror of the connector WIT `tool-value` variant.
#[derive(Debug, Clone, PartialEq)]
pub enum ToolValueOut {
    Text(String),
    Integer(i64),
    Decimal(f64),
    Boolean(bool),
}

/// Host-free mirror of the connector WIT `connector-error`, plus one extra
/// internal outcome — [`ToolError::ConfirmationRequired`] — that has no WIT
/// variant: the guest maps it onto `invalid-call` so the model is told to
/// re-invoke with `confirm=true`. Keeping it a distinct variant is what makes
/// "an unconfirmed mutation is never sent" directly assertable in native tests.
#[derive(Debug, Clone, PartialEq)]
pub enum ToolError {
    NotFound,
    InvalidCall(String),
    Unavailable,
    Failed(String),
    ConfirmationRequired(String),
}

/// A fully-planned HTTP request the guest hands to `oauth.authorized-request`.
/// `headers` never contains `authorization` — the host injects the bearer.
#[derive(Debug, Clone, PartialEq)]
pub struct PlannedRequest {
    pub method: String,
    pub url: String,
    pub headers: Vec<(String, String)>,
    pub body: Option<Vec<u8>>,
}

/// Host-free mirror of the connector WIT `tool-value`, used for an argument's
/// value.
#[derive(Debug, Clone, PartialEq)]
pub enum ArgValue {
    Text(String),
    Integer(i64),
    Decimal(f64),
    Boolean(bool),
}

/// Host-free mirror of the connector WIT `tool-argument`.
#[derive(Debug, Clone, PartialEq)]
pub struct Arg {
    pub name: String,
    pub value: ArgValue,
}

/// Host-free mirror of the connector WIT `tool-parameter`.
#[derive(Debug, Clone, PartialEq)]
pub struct ParamDef {
    pub name: String,
    pub value_type: String,
    pub required: bool,
}

/// Host-free mirror of the connector WIT `tool-definition`.
#[derive(Debug, Clone, PartialEq)]
pub struct ToolDef {
    pub name: String,
    pub description: String,
    pub parameters: Vec<ParamDef>,
}

// ---------------------------------------------------------------------------
// argument extraction helpers
// ---------------------------------------------------------------------------

fn find<'a>(args: &'a [Arg], name: &str) -> Option<&'a ArgValue> {
    args.iter().find(|a| a.name == name).map(|a| &a.value)
}

/// A non-empty text argument, or `None`. Only a `text` value counts.
fn opt_text(args: &[Arg], name: &str) -> Option<String> {
    match find(args, name) {
        Some(ArgValue::Text(s)) if !s.trim().is_empty() => Some(s.clone()),
        _ => None,
    }
}

/// A required non-empty text argument, or [`ToolError::InvalidCall`].
fn req_text(args: &[Arg], name: &str) -> Result<String, ToolError> {
    opt_text(args, name).ok_or_else(|| ToolError::InvalidCall(format!("missing required '{name}'")))
}

/// An integer argument. Accepts a WIT `integer`, or a `text` that parses as an
/// integer (the harness may forward either), or an integral `decimal`.
fn opt_integer(args: &[Arg], name: &str) -> Option<i64> {
    match find(args, name) {
        Some(ArgValue::Integer(i)) => Some(*i),
        Some(ArgValue::Text(s)) => s.trim().parse::<i64>().ok(),
        Some(ArgValue::Decimal(f)) if f.fract() == 0.0 => Some(*f as i64),
        _ => None,
    }
}

/// A required integer argument, or [`ToolError::InvalidCall`].
fn req_integer(args: &[Arg], name: &str) -> Result<i64, ToolError> {
    opt_integer(args, name)
        .ok_or_else(|| ToolError::InvalidCall(format!("missing or non-integer '{name}'")))
}

/// Whether a boolean argument is truthy. Accepts a WIT `boolean`, or a `text`
/// equal (case-insensitively) to `"true"`. Missing or anything else is `false`.
fn is_true(args: &[Arg], name: &str) -> bool {
    match find(args, name) {
        Some(ArgValue::Boolean(b)) => *b,
        Some(ArgValue::Text(s)) => s.trim().eq_ignore_ascii_case("true"),
        _ => false,
    }
}

/// An optional boolean argument: `Some(bool)` when present (as a WIT
/// `boolean` or a textual `"true"`/`"false"`), `None` when absent.
fn opt_bool(args: &[Arg], name: &str) -> Option<bool> {
    match find(args, name) {
        Some(ArgValue::Boolean(b)) => Some(*b),
        Some(ArgValue::Text(s)) if s.trim().eq_ignore_ascii_case("true") => Some(true),
        Some(ArgValue::Text(s)) if s.trim().eq_ignore_ascii_case("false") => Some(false),
        _ => None,
    }
}

/// The confirmation gate every mutating tool passes through FIRST. Returns
/// [`ToolError::ConfirmationRequired`] — and therefore builds no request — when
/// `confirm` is not truthy.
fn require_confirm(args: &[Arg], tool: &str) -> Result<(), ToolError> {
    if is_true(args, "confirm") {
        Ok(())
    } else {
        Err(ToolError::ConfirmationRequired(format!(
            "'{tool}' is a mutating Bitbucket operation; re-invoke with confirm=true to proceed"
        )))
    }
}

// ---------------------------------------------------------------------------
// URL builders
// ---------------------------------------------------------------------------

/// The repositories base for `workspace`, e.g.
/// `https://api.bitbucket.org/2.0/repositories/<workspace>`. `workspace` is
/// baked in as a path segment appended to the hardcoded [`API_BASE`] constant
/// — a hostile `workspace` value can inject extra path segments but can never
/// rewrite the scheme/host, since that prefix is a Rust string literal, not
/// derived from the argument.
fn repos_base(workspace: &str) -> String {
    format!("{API_BASE}/repositories/{workspace}")
}

/// The single-repository base for `workspace`/`repo_slug`, e.g.
/// `https://api.bitbucket.org/2.0/repositories/<workspace>/<repo_slug>`.
fn repo_base(workspace: &str, repo_slug: &str) -> String {
    format!("{}/{repo_slug}", repos_base(workspace))
}

/// Percent-encode every byte outside the URL "unreserved" set
/// (`ALPHA / DIGIT / "-" / "." / "_" / "~"`, RFC 3986 §2.3). Used for query
/// string values (`q`, `state`), which may contain BBQL syntax (spaces,
/// quotes, `=`, `~`). Deliberately conservative (encodes more than strictly
/// required) rather than risk leaving a query-string-breaking byte unescaped.
fn percent_encode(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    for byte in input.as_bytes() {
        let c = *byte as char;
        if c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.' | '~') {
            out.push(c);
        } else {
            out.push('%');
            out.push_str(&format!("{byte:02X}"));
        }
    }
    out
}

/// Append `?k=v&k2=v2...` (percent-encoding each value) to `base` from the
/// given `(key, value)` pairs, skipping any `None` value. Returns `base`
/// unchanged when every pair is `None`.
fn with_query(base: String, pairs: &[(&str, Option<String>)]) -> String {
    let mut url = base;
    let mut first = true;
    for (key, value) in pairs {
        if let Some(value) = value {
            url.push(if first { '?' } else { '&' });
            url.push_str(key);
            url.push('=');
            url.push_str(&percent_encode(value));
            first = false;
        }
    }
    url
}

// ---------------------------------------------------------------------------
// request/response builders
// ---------------------------------------------------------------------------

/// Standard Bitbucket request headers. `authorization` is deliberately
/// absent — the host injects the bearer for the selected profile.
fn base_headers(with_body: bool) -> Vec<(String, String)> {
    let mut headers = vec![("accept".to_string(), ACCEPT.to_string())];
    if with_body {
        headers.push(("content-type".to_string(), "application/json".to_string()));
    }
    headers
}

fn get(url: String) -> PlannedRequest {
    PlannedRequest {
        method: "GET".to_string(),
        url,
        headers: base_headers(false),
        body: None,
    }
}

fn with_json_body(method: &str, url: String, body: Value) -> Result<PlannedRequest, ToolError> {
    // A serde_json::Value always serializes in practice, but keep the
    // "component never panics on any input" invariant explicit rather than
    // relying on that: surface any failure as a tool error.
    let bytes = serde_json::to_vec(&body)
        .map_err(|e| ToolError::Failed(format!("failed to serialize request body: {e}")))?;
    Ok(PlannedRequest {
        method: method.to_string(),
        url,
        headers: base_headers(true),
        body: Some(bytes),
    })
}

// ---------------------------------------------------------------------------
// tool catalogue
// ---------------------------------------------------------------------------

fn param(name: &str, value_type: &str, required: bool) -> ParamDef {
    ParamDef {
        name: name.to_string(),
        value_type: value_type.to_string(),
        required,
    }
}

/// The `0.1.0` tool set the connector exports. Read-only tools carry no
/// `confirm`; every mutating tool carries a required boolean `confirm`.
pub fn tool_definitions() -> Vec<ToolDef> {
    let def = |name: &str, description: &str, parameters: Vec<ParamDef>| ToolDef {
        name: name.to_string(),
        description: description.to_string(),
        parameters,
    };
    let workspace = || param("workspace", "string", true);
    let repo_slug = || param("repo_slug", "string", true);
    let pull_request_id = || param("pull_request_id", "integer", true);
    let confirm = || param("confirm", "boolean", true);
    vec![
        def(
            "auth_status",
            "Report whether Bitbucket is connected, and which account the connection can reach.",
            vec![],
        ),
        def(
            "repo_list",
            "List repositories in a workspace. Requires workspace.",
            vec![
                workspace(),
                param("role", "string", false),
                param("q", "string", false),
            ],
        ),
        def(
            "repo_get",
            "Get one repository. Requires workspace and repo_slug.",
            vec![workspace(), repo_slug()],
        ),
        def(
            "pr_list",
            "List pull requests in a repository. Requires workspace and repo_slug.",
            vec![workspace(), repo_slug(), param("state", "string", false)],
        ),
        def(
            "issue_list",
            "List issues in a repository. Requires workspace and repo_slug.",
            vec![workspace(), repo_slug()],
        ),
        def(
            "pr_create",
            "Create a pull request. Mutating: requires confirm=true.",
            vec![
                workspace(),
                repo_slug(),
                param("title", "string", true),
                param("source_branch", "string", true),
                param("destination_branch", "string", true),
                param("description", "string", false),
                param("close_source_branch", "boolean", false),
                confirm(),
            ],
        ),
        def(
            "pr_merge",
            "Merge a pull request. Mutating: requires confirm=true.",
            vec![
                workspace(),
                repo_slug(),
                pull_request_id(),
                param("message", "string", false),
                param("merge_strategy", "string", false),
                param("close_source_branch", "boolean", false),
                confirm(),
            ],
        ),
        def(
            "issue_create",
            "Create an issue. Mutating: requires confirm=true.",
            vec![
                workspace(),
                repo_slug(),
                param("title", "string", true),
                param("kind", "string", false),
                param("priority", "string", false),
                param("content", "string", false),
                confirm(),
            ],
        ),
        def(
            "pr_comment",
            "Comment on a pull request. Mutating: requires confirm=true.",
            vec![
                workspace(),
                repo_slug(),
                pull_request_id(),
                param("body", "string", true),
                confirm(),
            ],
        ),
    ]
}

// ---------------------------------------------------------------------------
// planning
// ---------------------------------------------------------------------------

/// Build the HTTP request for `tool` from its `args`, or an error. A mutating
/// tool without `confirm=true` returns [`ToolError::ConfirmationRequired`] and
/// builds NO request.
pub fn plan_request(tool: &str, args: &[Arg]) -> Result<PlannedRequest, ToolError> {
    match tool {
        "auth_status" => Ok(get(format!("{API_BASE}/user"))),
        "repo_list" => {
            let workspace = req_text(args, "workspace")?;
            let url = with_query(
                repos_base(&workspace),
                &[("role", opt_text(args, "role")), ("q", opt_text(args, "q"))],
            );
            Ok(get(url))
        }
        "repo_get" => {
            let workspace = req_text(args, "workspace")?;
            let repo_slug = req_text(args, "repo_slug")?;
            Ok(get(repo_base(&workspace, &repo_slug)))
        }
        "pr_list" => {
            let workspace = req_text(args, "workspace")?;
            let repo_slug = req_text(args, "repo_slug")?;
            let url = with_query(
                format!("{}/pullrequests", repo_base(&workspace, &repo_slug)),
                &[("state", opt_text(args, "state"))],
            );
            Ok(get(url))
        }
        "issue_list" => {
            let workspace = req_text(args, "workspace")?;
            let repo_slug = req_text(args, "repo_slug")?;
            Ok(get(format!("{}/issues", repo_base(&workspace, &repo_slug))))
        }
        "pr_create" => {
            require_confirm(args, "pr_create")?;
            let workspace = req_text(args, "workspace")?;
            let repo_slug = req_text(args, "repo_slug")?;
            let title = req_text(args, "title")?;
            let source_branch = req_text(args, "source_branch")?;
            let destination_branch = req_text(args, "destination_branch")?;
            let mut body = Map::new();
            body.insert("title".to_string(), Value::String(title));
            body.insert(
                "source".to_string(),
                serde_json::json!({ "branch": { "name": source_branch } }),
            );
            body.insert(
                "destination".to_string(),
                serde_json::json!({ "branch": { "name": destination_branch } }),
            );
            if let Some(description) = opt_text(args, "description") {
                body.insert("description".to_string(), Value::String(description));
            }
            if let Some(close) = opt_bool(args, "close_source_branch") {
                body.insert("close_source_branch".to_string(), Value::Bool(close));
            }
            with_json_body(
                "POST",
                format!("{}/pullrequests", repo_base(&workspace, &repo_slug)),
                Value::Object(body),
            )
        }
        "pr_merge" => {
            require_confirm(args, "pr_merge")?;
            let workspace = req_text(args, "workspace")?;
            let repo_slug = req_text(args, "repo_slug")?;
            let pull_request_id = req_integer(args, "pull_request_id")?;
            let mut body = Map::new();
            if let Some(message) = opt_text(args, "message") {
                body.insert("message".to_string(), Value::String(message));
            }
            if let Some(strategy) = opt_text(args, "merge_strategy") {
                body.insert("merge_strategy".to_string(), Value::String(strategy));
            }
            if let Some(close) = opt_bool(args, "close_source_branch") {
                body.insert("close_source_branch".to_string(), Value::Bool(close));
            }
            with_json_body(
                "POST",
                format!(
                    "{}/pullrequests/{pull_request_id}/merge",
                    repo_base(&workspace, &repo_slug)
                ),
                Value::Object(body),
            )
        }
        "issue_create" => {
            require_confirm(args, "issue_create")?;
            let workspace = req_text(args, "workspace")?;
            let repo_slug = req_text(args, "repo_slug")?;
            let title = req_text(args, "title")?;
            let mut body = Map::new();
            body.insert("title".to_string(), Value::String(title));
            if let Some(kind) = opt_text(args, "kind") {
                body.insert("kind".to_string(), Value::String(kind));
            }
            if let Some(priority) = opt_text(args, "priority") {
                body.insert("priority".to_string(), Value::String(priority));
            }
            if let Some(content) = opt_text(args, "content") {
                body.insert("content".to_string(), serde_json::json!({ "raw": content }));
            }
            with_json_body(
                "POST",
                format!("{}/issues", repo_base(&workspace, &repo_slug)),
                Value::Object(body),
            )
        }
        "pr_comment" => {
            require_confirm(args, "pr_comment")?;
            let workspace = req_text(args, "workspace")?;
            let repo_slug = req_text(args, "repo_slug")?;
            let pull_request_id = req_integer(args, "pull_request_id")?;
            let text = req_text(args, "body")?;
            with_json_body(
                "POST",
                format!(
                    "{}/pullrequests/{pull_request_id}/comments",
                    repo_base(&workspace, &repo_slug)
                ),
                serde_json::json!({ "content": { "raw": text } }),
            )
        }
        other => Err(ToolError::InvalidCall(format!("unknown tool: {other}"))),
    }
}

// ---------------------------------------------------------------------------
// response parsing
// ---------------------------------------------------------------------------

/// Turn a tool's HTTP `(status, body)` into the connector value list, or an
/// error. `auth_status` is special: a `401`/`403` is reported as "not
/// connected" rather than raised as an error. `pr_merge` is special: a `202`
/// means Bitbucket queued the merge for asynchronous processing rather than
/// completing it synchronously.
pub fn parse_response(
    tool: &str,
    status: u16,
    body: &[u8],
) -> Result<Vec<ToolValueOut>, ToolError> {
    // `auth_status` never errors on an auth failure: it is a status probe.
    if tool == "auth_status" {
        return match status {
            200 => {
                let value = parse_json(body)?;
                let result = serde_json::json!({
                    "connected": true,
                    "account": summarize_account(&value),
                });
                Ok(text_value(&result))
            }
            401 | 403 => Ok(not_connected()),
            other => Err(status_error(other, body)),
        };
    }

    if tool == "pr_merge" && status == 202 {
        return Ok(text_value(&serde_json::json!({ "merging": true })));
    }

    if !(200..300).contains(&status) {
        return Err(match status {
            404 => ToolError::NotFound,
            other => status_error(other, body),
        });
    }

    match tool {
        "repo_list" => {
            let value = parse_json(body)?;
            let repositories: Vec<Value> = value
                .get("values")
                .and_then(Value::as_array)
                .map(|items| items.iter().map(summarize_repo).collect())
                .unwrap_or_default();
            let mut result = serde_json::json!({ "repositories": repositories });
            if let Some(next) = value.get("next") {
                result["next"] = next.clone();
            }
            Ok(text_value(&result))
        }
        "repo_get" => Ok(text_value(&summarize_repo(&parse_json(body)?))),
        "pr_list" => {
            let value = parse_json(body)?;
            let pull_requests: Vec<Value> = value
                .get("values")
                .and_then(Value::as_array)
                .map(|items| items.iter().map(summarize_pr).collect())
                .unwrap_or_default();
            let mut result = serde_json::json!({ "pull_requests": pull_requests });
            if let Some(next) = value.get("next") {
                result["next"] = next.clone();
            }
            Ok(text_value(&result))
        }
        "issue_list" => {
            let value = parse_json(body)?;
            let issues: Vec<Value> = value
                .get("values")
                .and_then(Value::as_array)
                .map(|items| items.iter().map(summarize_issue).collect())
                .unwrap_or_default();
            let mut result = serde_json::json!({ "issues": issues });
            if let Some(next) = value.get("next") {
                result["next"] = next.clone();
            }
            Ok(text_value(&result))
        }
        "pr_create" | "pr_merge" => Ok(text_value(&summarize_pr(&parse_json(body)?))),
        "issue_create" => Ok(text_value(&summarize_issue(&parse_json(body)?))),
        "pr_comment" => {
            let value = parse_json(body)?;
            let result = serde_json::json!({
                "id": value.get("id"),
                "url": value.get("links").and_then(|l| l.get("html")).and_then(|h| h.get("href")),
            });
            Ok(text_value(&result))
        }
        other => Err(ToolError::InvalidCall(format!("unknown tool: {other}"))),
    }
}

/// Parse a JSON body, mapping a parse failure to [`ToolError::Failed`].
fn parse_json(body: &[u8]) -> Result<Value, ToolError> {
    serde_json::from_slice(body)
        .map_err(|e| ToolError::Failed(format!("Bitbucket response is not JSON: {e}")))
}

/// Wrap a JSON value as a single-element connector text value list.
fn text_value(value: &Value) -> Vec<ToolValueOut> {
    vec![ToolValueOut::Text(value.to_string())]
}

/// A generic non-2xx failure, carrying the status and Bitbucket's own error
/// message when the body is JSON.
fn status_error(status: u16, body: &[u8]) -> ToolError {
    match bitbucket_message(body) {
        Some(message) if !message.is_empty() => {
            ToolError::Failed(format!("Bitbucket API error: HTTP {status}: {message}"))
        }
        _ => ToolError::Failed(format!("Bitbucket API error: HTTP {status}")),
    }
}

/// The error message from a Bitbucket JSON error body
/// (`{"type":"error","error":{"message":"..."}}`), if any.
fn bitbucket_message(body: &[u8]) -> Option<String> {
    let value: Value = serde_json::from_slice(body).ok()?;
    value
        .get("error")
        .and_then(|e| e.get("message"))
        .and_then(Value::as_str)
        .map(str::to_string)
}

fn summarize_account(value: &Value) -> Value {
    serde_json::json!({
        "uuid": value.get("uuid"),
        "display_name": value.get("display_name"),
    })
}

fn summarize_repo(value: &Value) -> Value {
    serde_json::json!({
        "uuid": value.get("uuid"),
        "full_name": value.get("full_name"),
        "name": value.get("name"),
        "is_private": value.get("is_private"),
        "description": value.get("description"),
        "url": value.get("links").and_then(|l| l.get("html")).and_then(|h| h.get("href")),
    })
}

fn summarize_pr(value: &Value) -> Value {
    serde_json::json!({
        "id": value.get("id"),
        "title": value.get("title"),
        "state": value.get("state"),
        "source_branch": value
            .get("source")
            .and_then(|s| s.get("branch"))
            .and_then(|b| b.get("name")),
        "destination_branch": value
            .get("destination")
            .and_then(|d| d.get("branch"))
            .and_then(|b| b.get("name")),
        "author": value.get("author").and_then(|a| a.get("display_name")),
        "merge_commit": value
            .get("merge_commit")
            .and_then(|m| m.get("hash")),
        "url": value.get("links").and_then(|l| l.get("html")).and_then(|h| h.get("href")),
    })
}

fn summarize_issue(value: &Value) -> Value {
    serde_json::json!({
        "id": value.get("id"),
        "title": value.get("title"),
        "state": value.get("state"),
        "kind": value.get("kind"),
        "priority": value.get("priority"),
        "url": value.get("links").and_then(|l| l.get("html")).and_then(|h| h.get("href")),
    })
}

/// The connector value list a "not connected" `auth_status` reports. Shared by
/// the `401`/`403` response path and the guest's `denied`/`expired` OAuth
/// path, so a missing/expired token never surfaces as a tool error.
pub fn not_connected() -> Vec<ToolValueOut> {
    let value = serde_json::json!({
        "connected": false,
        "message": "Bitbucket is not connected. Connect it via Cockpit's Plugins screen."
    });
    vec![ToolValueOut::Text(value.to_string())]
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn t(name: &str, value: &str) -> Arg {
        Arg {
            name: name.to_string(),
            value: ArgValue::Text(value.to_string()),
        }
    }
    fn i(name: &str, value: i64) -> Arg {
        Arg {
            name: name.to_string(),
            value: ArgValue::Integer(value),
        }
    }
    fn b(name: &str, value: bool) -> Arg {
        Arg {
            name: name.to_string(),
            value: ArgValue::Boolean(value),
        }
    }

    /// The body of a `PlannedRequest` parsed back into JSON for assertions.
    fn body_json(req: &PlannedRequest) -> Value {
        serde_json::from_slice(req.body.as_ref().expect("expected a body")).unwrap()
    }

    fn header<'a>(req: &'a PlannedRequest, name: &str) -> Option<&'a str> {
        req.headers
            .iter()
            .find(|(n, _)| n == name)
            .map(|(_, v)| v.as_str())
    }

    const WORKSPACE: &str = "acme";
    const REPO: &str = "widgets";

    /// Every URL a planned request targets must resolve to
    /// `api.bitbucket.org` — the only host the manifest authorizes for
    /// actual tool traffic.
    fn assert_confined_to_api_bitbucket_org(req: &PlannedRequest) {
        assert!(
            req.url.starts_with("https://api.bitbucket.org/2.0/"),
            "planned request left api.bitbucket.org: {}",
            req.url
        );
    }

    // ---------------- tool catalogue ----------------

    #[test]
    fn tool_catalogue_has_the_expected_0_1_0_tools() {
        let names: Vec<String> = tool_definitions().into_iter().map(|d| d.name).collect();
        assert_eq!(
            names,
            vec![
                "auth_status",
                "repo_list",
                "repo_get",
                "pr_list",
                "issue_list",
                "pr_create",
                "pr_merge",
                "issue_create",
                "pr_comment",
            ]
        );
    }

    #[test]
    fn every_mutating_tool_declares_a_required_confirm_boolean() {
        let defs = tool_definitions();
        for name in ["pr_create", "pr_merge", "issue_create", "pr_comment"] {
            let def = defs.iter().find(|d| d.name == name).unwrap();
            let confirm = def
                .parameters
                .iter()
                .find(|p| p.name == "confirm")
                .unwrap_or_else(|| panic!("{name} must declare a confirm parameter"));
            assert_eq!(confirm.value_type, "boolean", "{name}.confirm type");
            assert!(confirm.required, "{name}.confirm must be required");
        }
    }

    #[test]
    fn read_only_tools_have_no_confirm_parameter() {
        let defs = tool_definitions();
        for name in [
            "auth_status",
            "repo_list",
            "repo_get",
            "pr_list",
            "issue_list",
        ] {
            let def = defs.iter().find(|d| d.name == name).unwrap();
            assert!(
                def.parameters.iter().all(|p| p.name != "confirm"),
                "{name} must not gate on confirm"
            );
        }
    }

    // ---------------- read-only planning ----------------

    #[test]
    fn auth_status_gets_the_current_user() {
        let req = plan_request("auth_status", &[]).unwrap();
        assert_eq!(req.method, "GET");
        assert_eq!(req.url, "https://api.bitbucket.org/2.0/user");
        assert!(req.body.is_none());
        assert_eq!(header(&req, "accept"), Some(ACCEPT));
        // The component never sets its own authorization; the host injects it.
        assert!(header(&req, "authorization").is_none());
        assert_confined_to_api_bitbucket_org(&req);
    }

    #[test]
    fn repo_list_targets_the_workspace_repositories_endpoint() {
        let req = plan_request("repo_list", &[t("workspace", WORKSPACE)]).unwrap();
        assert_eq!(req.method, "GET");
        assert_eq!(
            req.url,
            format!("https://api.bitbucket.org/2.0/repositories/{WORKSPACE}")
        );
        assert_confined_to_api_bitbucket_org(&req);
        assert!(header(&req, "authorization").is_none());
    }

    #[test]
    fn repo_list_forwards_role_and_q() {
        let req = plan_request(
            "repo_list",
            &[
                t("workspace", WORKSPACE),
                t("role", "admin"),
                t("q", "name ~ \"widgets\""),
            ],
        )
        .unwrap();
        assert!(req.url.contains("?role=admin&q="));
        assert!(req.url.contains("name"));
        assert!(!req.url.contains(' '), "query value must be encoded");
    }

    #[test]
    fn repo_list_requires_workspace() {
        let err = plan_request("repo_list", &[]).unwrap_err();
        assert!(matches!(err, ToolError::InvalidCall(_)));
    }

    #[test]
    fn repo_get_targets_the_single_repository_endpoint() {
        let req = plan_request(
            "repo_get",
            &[t("workspace", WORKSPACE), t("repo_slug", REPO)],
        )
        .unwrap();
        assert_eq!(req.method, "GET");
        assert_eq!(
            req.url,
            format!("https://api.bitbucket.org/2.0/repositories/{WORKSPACE}/{REPO}")
        );
        assert_confined_to_api_bitbucket_org(&req);
    }

    #[test]
    fn pr_list_targets_the_pullrequests_endpoint() {
        let req = plan_request(
            "pr_list",
            &[t("workspace", WORKSPACE), t("repo_slug", REPO)],
        )
        .unwrap();
        assert_eq!(req.method, "GET");
        assert_eq!(
            req.url,
            format!("https://api.bitbucket.org/2.0/repositories/{WORKSPACE}/{REPO}/pullrequests")
        );
        assert_confined_to_api_bitbucket_org(&req);
    }

    #[test]
    fn pr_list_forwards_state() {
        let req = plan_request(
            "pr_list",
            &[
                t("workspace", WORKSPACE),
                t("repo_slug", REPO),
                t("state", "OPEN"),
            ],
        )
        .unwrap();
        assert!(req.url.ends_with("?state=OPEN"));
    }

    #[test]
    fn issue_list_targets_the_issues_endpoint() {
        let req = plan_request(
            "issue_list",
            &[t("workspace", WORKSPACE), t("repo_slug", REPO)],
        )
        .unwrap();
        assert_eq!(req.method, "GET");
        assert_eq!(
            req.url,
            format!("https://api.bitbucket.org/2.0/repositories/{WORKSPACE}/{REPO}/issues")
        );
        assert_confined_to_api_bitbucket_org(&req);
    }

    #[test]
    fn a_hostile_workspace_cannot_rewrite_the_host() {
        // workspace is baked into the URL as a path segment appended to the
        // hardcoded API_BASE constant, so even a value shaped like a full
        // authority can never escape api.bitbucket.org.
        let hostile = "evil.example.com";
        let req = plan_request("repo_list", &[t("workspace", hostile)]).unwrap();
        assert_confined_to_api_bitbucket_org(&req);
        assert_eq!(
            req.url,
            format!("https://api.bitbucket.org/2.0/repositories/{hostile}")
        );
    }

    // ---------------- the confirmation gate ----------------

    /// The central "an unapproved request is not sent" guarantee: every
    /// mutating tool, called WITHOUT confirm, must yield ConfirmationRequired
    /// and never a PlannedRequest.
    #[test]
    fn every_mutation_without_confirm_is_refused_before_any_request() {
        let cases: Vec<(&str, Vec<Arg>)> = vec![
            (
                "pr_create",
                vec![
                    t("workspace", WORKSPACE),
                    t("repo_slug", REPO),
                    t("title", "Fix the thing"),
                    t("source_branch", "feature/fix"),
                    t("destination_branch", "main"),
                ],
            ),
            (
                "pr_merge",
                vec![
                    t("workspace", WORKSPACE),
                    t("repo_slug", REPO),
                    i("pull_request_id", 42),
                ],
            ),
            (
                "issue_create",
                vec![
                    t("workspace", WORKSPACE),
                    t("repo_slug", REPO),
                    t("title", "Bug report"),
                ],
            ),
            (
                "pr_comment",
                vec![
                    t("workspace", WORKSPACE),
                    t("repo_slug", REPO),
                    i("pull_request_id", 42),
                    t("body", "hi"),
                ],
            ),
        ];
        for (tool, args) in cases {
            let result = plan_request(tool, &args);
            assert!(
                matches!(result, Err(ToolError::ConfirmationRequired(_))),
                "{tool} without confirm must be ConfirmationRequired, got {result:?}"
            );
        }
    }

    #[test]
    fn confirm_gate_accepts_a_textual_true() {
        // The harness normally sends a JSON bool, but a stringified "true" is
        // accepted defensively so a mutation is not wrongly blocked.
        let req = plan_request(
            "pr_comment",
            &[
                t("workspace", WORKSPACE),
                t("repo_slug", REPO),
                i("pull_request_id", 42),
                t("body", "hi"),
                t("confirm", "true"),
            ],
        )
        .unwrap();
        assert_eq!(req.method, "POST");
    }

    // ---------------- mutating planning (confirmed) ----------------

    #[test]
    fn pr_create_posts_title_source_and_destination() {
        let req = plan_request(
            "pr_create",
            &[
                t("workspace", WORKSPACE),
                t("repo_slug", REPO),
                t("title", "Fix the thing"),
                t("source_branch", "feature/fix"),
                t("destination_branch", "main"),
                t("description", "Fixes the thing."),
                b("close_source_branch", true),
                b("confirm", true),
            ],
        )
        .unwrap();
        assert_eq!(req.method, "POST");
        assert_eq!(
            req.url,
            format!("https://api.bitbucket.org/2.0/repositories/{WORKSPACE}/{REPO}/pullrequests")
        );
        let body = body_json(&req);
        assert_eq!(body["title"], "Fix the thing");
        assert_eq!(body["source"]["branch"]["name"], "feature/fix");
        assert_eq!(body["destination"]["branch"]["name"], "main");
        assert_eq!(body["description"], "Fixes the thing.");
        assert_eq!(body["close_source_branch"], true);
        assert_confined_to_api_bitbucket_org(&req);
        assert!(header(&req, "authorization").is_none());
    }

    #[test]
    fn pr_create_omits_optional_fields_when_absent() {
        let req = plan_request(
            "pr_create",
            &[
                t("workspace", WORKSPACE),
                t("repo_slug", REPO),
                t("title", "Fix the thing"),
                t("source_branch", "feature/fix"),
                t("destination_branch", "main"),
                b("confirm", true),
            ],
        )
        .unwrap();
        let body = body_json(&req);
        assert!(body.get("description").is_none());
        assert!(body.get("close_source_branch").is_none());
    }

    #[test]
    fn pr_create_requires_title_source_and_destination_branch() {
        let err = plan_request(
            "pr_create",
            &[
                t("workspace", WORKSPACE),
                t("repo_slug", REPO),
                b("confirm", true),
            ],
        )
        .unwrap_err();
        assert!(matches!(err, ToolError::InvalidCall(_)));
    }

    #[test]
    fn pr_merge_posts_to_the_merge_endpoint() {
        let req = plan_request(
            "pr_merge",
            &[
                t("workspace", WORKSPACE),
                t("repo_slug", REPO),
                i("pull_request_id", 42),
                t("message", "Merging via Ryuzi"),
                t("merge_strategy", "squash"),
                b("close_source_branch", true),
                b("confirm", true),
            ],
        )
        .unwrap();
        assert_eq!(req.method, "POST");
        assert_eq!(
            req.url,
            format!(
                "https://api.bitbucket.org/2.0/repositories/{WORKSPACE}/{REPO}/pullrequests/42/merge"
            )
        );
        let body = body_json(&req);
        assert_eq!(body["message"], "Merging via Ryuzi");
        assert_eq!(body["merge_strategy"], "squash");
        assert_eq!(body["close_source_branch"], true);
        assert_confined_to_api_bitbucket_org(&req);
    }

    #[test]
    fn pr_merge_sends_an_empty_body_when_no_optional_fields_are_given() {
        let req = plan_request(
            "pr_merge",
            &[
                t("workspace", WORKSPACE),
                t("repo_slug", REPO),
                i("pull_request_id", 42),
                b("confirm", true),
            ],
        )
        .unwrap();
        assert_eq!(body_json(&req), json!({}));
    }

    #[test]
    fn pr_merge_requires_pull_request_id() {
        let err = plan_request(
            "pr_merge",
            &[
                t("workspace", WORKSPACE),
                t("repo_slug", REPO),
                b("confirm", true),
            ],
        )
        .unwrap_err();
        assert!(matches!(err, ToolError::InvalidCall(_)));
    }

    #[test]
    fn issue_create_posts_title_kind_priority_and_content() {
        let req = plan_request(
            "issue_create",
            &[
                t("workspace", WORKSPACE),
                t("repo_slug", REPO),
                t("title", "Bug report"),
                t("kind", "bug"),
                t("priority", "major"),
                t("content", "Steps to reproduce..."),
                b("confirm", true),
            ],
        )
        .unwrap();
        assert_eq!(req.method, "POST");
        assert_eq!(
            req.url,
            format!("https://api.bitbucket.org/2.0/repositories/{WORKSPACE}/{REPO}/issues")
        );
        let body = body_json(&req);
        assert_eq!(body["title"], "Bug report");
        assert_eq!(body["kind"], "bug");
        assert_eq!(body["priority"], "major");
        assert_eq!(body["content"]["raw"], "Steps to reproduce...");
    }

    #[test]
    fn issue_create_requires_title() {
        let err = plan_request(
            "issue_create",
            &[
                t("workspace", WORKSPACE),
                t("repo_slug", REPO),
                b("confirm", true),
            ],
        )
        .unwrap_err();
        assert!(matches!(err, ToolError::InvalidCall(_)));
    }

    #[test]
    fn pr_comment_posts_content_raw_to_the_comments_endpoint() {
        let req = plan_request(
            "pr_comment",
            &[
                t("workspace", WORKSPACE),
                t("repo_slug", REPO),
                i("pull_request_id", 42),
                t("body", "Looks good to me."),
                b("confirm", true),
            ],
        )
        .unwrap();
        assert_eq!(req.method, "POST");
        assert_eq!(
            req.url,
            format!(
                "https://api.bitbucket.org/2.0/repositories/{WORKSPACE}/{REPO}/pullrequests/42/comments"
            )
        );
        let body = body_json(&req);
        assert_eq!(body["content"]["raw"], "Looks good to me.");
    }

    #[test]
    fn unknown_tool_is_invalid_call() {
        let err = plan_request("delete_everything", &[]).unwrap_err();
        assert!(matches!(err, ToolError::InvalidCall(_)));
    }

    // ---------------- response parsing ----------------

    #[test]
    fn auth_status_200_reports_connected_account() {
        let body = br#"{"uuid":"{4b4a-...}","display_name":"Ada Lovelace"}"#;
        let values = parse_response("auth_status", 200, body).unwrap();
        let ToolValueOut::Text(text) = &values[0] else {
            panic!("expected a text value");
        };
        let parsed: Value = serde_json::from_str(text).unwrap();
        assert_eq!(parsed["connected"], true);
        assert_eq!(parsed["account"]["display_name"], "Ada Lovelace");
    }

    #[test]
    fn auth_status_401_reports_not_connected_without_erroring() {
        let values = parse_response(
            "auth_status",
            401,
            br#"{"type":"error","error":{"message":"unauthorized"}}"#,
        )
        .unwrap();
        let ToolValueOut::Text(text) = &values[0] else {
            panic!("expected a text value");
        };
        let parsed: Value = serde_json::from_str(text).unwrap();
        assert_eq!(parsed["connected"], false);
    }

    #[test]
    fn repo_list_200_summarizes_each_repository() {
        let body = br#"{
            "values":[
                {"uuid":"{repo-1}","full_name":"acme/widgets","name":"widgets","is_private":true,"description":"Widgets","links":{"html":{"href":"https://bitbucket.org/acme/widgets"}}}
            ],
            "next":"https://api.bitbucket.org/2.0/repositories/acme?page=2"
        }"#;
        let values = parse_response("repo_list", 200, body).unwrap();
        let ToolValueOut::Text(text) = &values[0] else {
            panic!("expected a text value");
        };
        let parsed: Value = serde_json::from_str(text).unwrap();
        assert_eq!(parsed["repositories"][0]["full_name"], "acme/widgets");
        assert_eq!(parsed["repositories"][0]["is_private"], true);
        assert_eq!(
            parsed["repositories"][0]["url"],
            "https://bitbucket.org/acme/widgets"
        );
        assert_eq!(
            parsed["next"],
            "https://api.bitbucket.org/2.0/repositories/acme?page=2"
        );
    }

    #[test]
    fn repo_get_200_summarizes_the_repository() {
        let body = br#"{"uuid":"{repo-1}","full_name":"acme/widgets","name":"widgets","is_private":false,"description":null,"links":{"html":{"href":"https://bitbucket.org/acme/widgets"}}}"#;
        let values = parse_response("repo_get", 200, body).unwrap();
        let ToolValueOut::Text(text) = &values[0] else {
            panic!("expected a text value");
        };
        let parsed: Value = serde_json::from_str(text).unwrap();
        assert_eq!(parsed["full_name"], "acme/widgets");
        assert_eq!(parsed["is_private"], false);
    }

    #[test]
    fn repo_get_404_is_not_found() {
        let err = parse_response("repo_get", 404, b"{}").unwrap_err();
        assert!(matches!(err, ToolError::NotFound));
    }

    #[test]
    fn pr_list_200_summarizes_each_pull_request() {
        let body = br#"{
            "values":[
                {
                    "id":42,"title":"Fix the thing","state":"OPEN",
                    "source":{"branch":{"name":"feature/fix"}},
                    "destination":{"branch":{"name":"main"}},
                    "author":{"display_name":"Ada Lovelace"},
                    "links":{"html":{"href":"https://bitbucket.org/acme/widgets/pull-requests/42"}}
                }
            ]
        }"#;
        let values = parse_response("pr_list", 200, body).unwrap();
        let ToolValueOut::Text(text) = &values[0] else {
            panic!("expected a text value");
        };
        let parsed: Value = serde_json::from_str(text).unwrap();
        assert_eq!(parsed["pull_requests"][0]["id"], 42);
        assert_eq!(parsed["pull_requests"][0]["title"], "Fix the thing");
        assert_eq!(parsed["pull_requests"][0]["state"], "OPEN");
        assert_eq!(parsed["pull_requests"][0]["source_branch"], "feature/fix");
        assert_eq!(parsed["pull_requests"][0]["destination_branch"], "main");
        assert_eq!(parsed["pull_requests"][0]["author"], "Ada Lovelace");
    }

    #[test]
    fn issue_list_200_summarizes_each_issue() {
        let body = br#"{
            "values":[
                {"id":7,"title":"Bug report","state":"new","kind":"bug","priority":"major","links":{"html":{"href":"https://bitbucket.org/acme/widgets/issues/7"}}}
            ]
        }"#;
        let values = parse_response("issue_list", 200, body).unwrap();
        let ToolValueOut::Text(text) = &values[0] else {
            panic!("expected a text value");
        };
        let parsed: Value = serde_json::from_str(text).unwrap();
        assert_eq!(parsed["issues"][0]["id"], 7);
        assert_eq!(parsed["issues"][0]["title"], "Bug report");
        assert_eq!(parsed["issues"][0]["kind"], "bug");
        assert_eq!(parsed["issues"][0]["priority"], "major");
    }

    #[test]
    fn pr_create_201_returns_the_new_pull_request() {
        let body = br#"{
            "id":43,"title":"Fix the thing","state":"OPEN",
            "source":{"branch":{"name":"feature/fix"}},
            "destination":{"branch":{"name":"main"}},
            "links":{"html":{"href":"https://bitbucket.org/acme/widgets/pull-requests/43"}}
        }"#;
        let values = parse_response("pr_create", 201, body).unwrap();
        let ToolValueOut::Text(text) = &values[0] else {
            panic!("expected a text value");
        };
        let parsed: Value = serde_json::from_str(text).unwrap();
        assert_eq!(parsed["id"], 43);
        assert_eq!(parsed["state"], "OPEN");
        assert_eq!(
            parsed["url"],
            "https://bitbucket.org/acme/widgets/pull-requests/43"
        );
    }

    #[test]
    fn pr_merge_200_reports_the_merge_commit() {
        let body = br#"{
            "id":42,"title":"Fix the thing","state":"MERGED",
            "source":{"branch":{"name":"feature/fix"}},
            "destination":{"branch":{"name":"main"}},
            "merge_commit":{"hash":"abc1234"},
            "links":{"html":{"href":"https://bitbucket.org/acme/widgets/pull-requests/42"}}
        }"#;
        let values = parse_response("pr_merge", 200, body).unwrap();
        let ToolValueOut::Text(text) = &values[0] else {
            panic!("expected a text value");
        };
        let parsed: Value = serde_json::from_str(text).unwrap();
        assert_eq!(parsed["state"], "MERGED");
        assert_eq!(parsed["merge_commit"], "abc1234");
    }

    #[test]
    fn pr_merge_202_reports_an_async_merge_without_a_body() {
        let values = parse_response("pr_merge", 202, b"").unwrap();
        let ToolValueOut::Text(text) = &values[0] else {
            panic!("expected a text value");
        };
        let parsed: Value = serde_json::from_str(text).unwrap();
        assert_eq!(parsed["merging"], true);
    }

    #[test]
    fn issue_create_201_returns_the_new_issue() {
        let body = br#"{"id":8,"title":"Bug report","state":"new","kind":"bug","priority":"major","links":{"html":{"href":"https://bitbucket.org/acme/widgets/issues/8"}}}"#;
        let values = parse_response("issue_create", 201, body).unwrap();
        let ToolValueOut::Text(text) = &values[0] else {
            panic!("expected a text value");
        };
        let parsed: Value = serde_json::from_str(text).unwrap();
        assert_eq!(parsed["id"], 8);
        assert_eq!(parsed["kind"], "bug");
    }

    #[test]
    fn pr_comment_201_returns_id_and_url() {
        let body = br#"{"id":99,"content":{"raw":"Looks good to me."},"links":{"html":{"href":"https://bitbucket.org/acme/widgets/pull-requests/42/_/diff#comment-99"}}}"#;
        let values = parse_response("pr_comment", 201, body).unwrap();
        let ToolValueOut::Text(text) = &values[0] else {
            panic!("expected a text value");
        };
        let parsed: Value = serde_json::from_str(text).unwrap();
        assert_eq!(parsed["id"], 99);
        assert_eq!(
            parsed["url"],
            "https://bitbucket.org/acme/widgets/pull-requests/42/_/diff#comment-99"
        );
    }

    #[test]
    fn a_500_is_a_failure_carrying_the_status() {
        let err = parse_response("repo_get", 500, b"boom").unwrap_err();
        match err {
            ToolError::Failed(message) => assert!(message.contains("500")),
            other => panic!("expected Failed, got {other:?}"),
        }
    }

    #[test]
    fn a_bitbucket_error_message_is_surfaced() {
        let err = parse_response(
            "repo_get",
            400,
            br#"{"type":"error","error":{"message":"Invalid request"}}"#,
        )
        .unwrap_err();
        match err {
            ToolError::Failed(message) => assert!(message.contains("Invalid request")),
            other => panic!("expected Failed, got {other:?}"),
        }
    }

    #[test]
    fn not_connected_helper_reports_disconnected() {
        let ToolValueOut::Text(text) = &not_connected()[0] else {
            panic!("expected a text value");
        };
        let parsed: Value = serde_json::from_str(text).unwrap();
        assert_eq!(parsed["connected"], false);
    }

    // ---------------- helper unit tests ----------------

    #[test]
    fn percent_encode_escapes_spaces_and_equals_and_quotes() {
        assert_eq!(percent_encode("a b"), "a%20b");
        assert_eq!(percent_encode("x=y"), "x%3Dy");
        assert_eq!(percent_encode(r#"title~"Bug""#), "title~%22Bug%22");
        assert_eq!(
            percent_encode("unreserved-._~ABC123"),
            "unreserved-._~ABC123"
        );
    }
}
