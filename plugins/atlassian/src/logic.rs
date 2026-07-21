//! Pure, host-free Atlassian connector logic. Every function here is
//! deterministic over its inputs (no network, no clock, no storage), so the
//! whole module is covered by native `cargo test`. The wasm `guest` glue
//! supplies the live effect — one `oauth.authorized-request("atlassian-cloud",
//! ..)` per planned request — and maps these plain types to/from WIT.
//!
//! # Endpoints (verified against developer.atlassian.com, July 2026)
//! Every Jira/Confluence call goes through the OAuth 2.0 (3LO) API gateway:
//! `https://api.atlassian.com/ex/jira/{cloud_id}/rest/api/3/...` and
//! `https://api.atlassian.com/ex/confluence/{cloud_id}/wiki/...` — never a
//! tenant's own `*.atlassian.net` host. `{cloud_id}` comes from `GET
//! https://api.atlassian.com/oauth/token/accessible-resources` (the
//! `auth_status` tool). Jira issue search uses `/rest/api/3/search/jql`, NOT
//! the legacy `/rest/api/3/search` — Atlassian removed that endpoint (HTTP
//! `410 Gone`) between May and October 2025. Confluence page CRUD uses the
//! current v2 API (`/wiki/api/v2/pages`, keyed by numeric `spaceId`/`pageId`);
//! full-text/CQL search stays on the still-current v1 `/wiki/rest/api/search`
//! endpoint, which has no v2 equivalent.
//!
//! # Why `cloud_id` is a plain argument
//! See the crate root doc: this bundle's `lifecycle` is `per-call`, so a
//! resolve-and-cache approach has nowhere to keep the cached value between
//! one `invoke` and the next. Every Jira/Confluence tool therefore takes an
//! explicit `cloud_id` argument, and `auth_status` is how a caller discovers
//! which ids a connection can reach.
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

/// Atlassian's OAuth 2.0 (3LO) API gateway. Every planned request targets
/// this host, which the manifest's `api.atlassian.com` network entry
/// authorizes. Individual tenants' own `*.atlassian.net` hosts are never
/// addressed directly.
pub const API_BASE: &str = "https://api.atlassian.com";

/// The OAuth profile id the guest passes to `authorized-request` — matches
/// the `[[oauth]] id` in `ryuzi-plugin.toml`. ONE profile serves both Jira
/// and Confluence tools (see the crate root doc).
pub const OAUTH_PROFILE: &str = "atlassian-cloud";

/// The `Accept` media type every Atlassian Cloud REST call uses.
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

/// The confirmation gate every mutating tool passes through FIRST. Returns
/// [`ToolError::ConfirmationRequired`] — and therefore builds no request — when
/// `confirm` is not truthy.
fn require_confirm(args: &[Arg], tool: &str) -> Result<(), ToolError> {
    if is_true(args, "confirm") {
        Ok(())
    } else {
        Err(ToolError::ConfirmationRequired(format!(
            "'{tool}' is a mutating Atlassian operation; re-invoke with confirm=true to proceed"
        )))
    }
}

// ---------------------------------------------------------------------------
// URL builders
// ---------------------------------------------------------------------------

/// The Jira Cloud REST v3 base for `cloud_id`, e.g.
/// `https://api.atlassian.com/ex/jira/<cloud_id>/rest/api/3`.
fn jira_base(cloud_id: &str) -> String {
    format!("{API_BASE}/ex/jira/{cloud_id}/rest/api/3")
}

/// The Confluence Cloud REST v1 base for `cloud_id` (CQL search only), e.g.
/// `https://api.atlassian.com/ex/confluence/<cloud_id>/wiki/rest/api`.
fn confluence_v1_base(cloud_id: &str) -> String {
    format!("{API_BASE}/ex/confluence/{cloud_id}/wiki/rest/api")
}

/// The Confluence Cloud REST v2 base for `cloud_id` (page CRUD), e.g.
/// `https://api.atlassian.com/ex/confluence/<cloud_id>/wiki/api/v2`.
fn confluence_v2_base(cloud_id: &str) -> String {
    format!("{API_BASE}/ex/confluence/{cloud_id}/wiki/api/v2")
}

/// Percent-encode every byte outside the URL "unreserved" set
/// (`ALPHA / DIGIT / "-" / "." / "_" / "~"`, RFC 3986 §2.3) as `%XX`. Used for
/// a CQL query embedded in a `?cql=...` query-string value, which routinely
/// contains spaces, quotes and `=`. Deliberately conservative (encodes more
/// than strictly required, e.g. it also escapes `/`) rather than risk leaving
/// a query-string-breaking byte unescaped.
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

/// The minimal Atlassian Document Format (ADF) document wrapping `text` as a
/// single paragraph — the shape Jira's `description`/comment `body` fields
/// require in the v3 API (a bare string is rejected).
fn adf_doc(text: &str) -> Value {
    serde_json::json!({
        "type": "doc",
        "version": 1,
        "content": [
            { "type": "paragraph", "content": [ { "type": "text", "text": text } ] }
        ]
    })
}

// ---------------------------------------------------------------------------
// request/response builders
// ---------------------------------------------------------------------------

/// Standard Atlassian request headers. `authorization` is deliberately
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
    let cloud_id = || param("cloud_id", "string", true);
    let confirm = || param("confirm", "boolean", true);
    vec![
        def(
            "auth_status",
            "Report whether Atlassian is connected, and which Jira/Confluence sites (cloud_id, name, url) the connection can reach.",
            vec![],
        ),
        def(
            "jira_search",
            "Search Jira issues by JQL. Requires cloud_id (see auth_status).",
            vec![
                cloud_id(),
                param("jql", "string", true),
                param("max_results", "integer", false),
            ],
        ),
        def(
            "jira_issue_get",
            "Get one Jira issue by key (e.g. PROJ-123). Requires cloud_id.",
            vec![cloud_id(), param("issue_key", "string", true)],
        ),
        def(
            "jira_issue_create",
            "Create a Jira issue. Mutating: requires confirm=true.",
            vec![
                cloud_id(),
                param("project_key", "string", true),
                param("issue_type", "string", true),
                param("summary", "string", true),
                param("description", "string", false),
                confirm(),
            ],
        ),
        def(
            "jira_issue_comment",
            "Comment on a Jira issue. Mutating: requires confirm=true.",
            vec![
                cloud_id(),
                param("issue_key", "string", true),
                param("body", "string", true),
                confirm(),
            ],
        ),
        def(
            "jira_issue_transition",
            "Transition a Jira issue to a new status by transition id (see the issue's available transitions). Mutating: requires confirm=true.",
            vec![
                cloud_id(),
                param("issue_key", "string", true),
                param("transition_id", "string", true),
                confirm(),
            ],
        ),
        def(
            "confluence_search",
            "Search Confluence content by CQL. Requires cloud_id.",
            vec![
                cloud_id(),
                param("cql", "string", true),
                param("limit", "integer", false),
            ],
        ),
        def(
            "confluence_page_get",
            "Get one Confluence page by numeric page id, including its storage-format body. Requires cloud_id.",
            vec![cloud_id(), param("page_id", "string", true)],
        ),
        def(
            "confluence_page_create",
            "Create a Confluence page (storage-format HTML body) in a space. Mutating: requires confirm=true.",
            vec![
                cloud_id(),
                param("space_id", "string", true),
                param("title", "string", true),
                param("body", "string", true),
                param("parent_id", "string", false),
                confirm(),
            ],
        ),
        def(
            "confluence_page_update",
            "Update a Confluence page's title/body. `version` must be the NEW version number (current version + 1). Mutating: requires confirm=true.",
            vec![
                cloud_id(),
                param("page_id", "string", true),
                param("title", "string", true),
                param("body", "string", true),
                param("version", "integer", true),
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
        "auth_status" => Ok(get(format!("{API_BASE}/oauth/token/accessible-resources"))),
        "jira_search" => {
            let cloud_id = req_text(args, "cloud_id")?;
            let jql = req_text(args, "jql")?;
            let mut body = Map::new();
            body.insert("jql".to_string(), Value::String(jql));
            body.insert(
                "fields".to_string(),
                serde_json::json!(["summary", "status", "assignee"]),
            );
            if let Some(max_results) = opt_integer(args, "max_results") {
                body.insert("maxResults".to_string(), serde_json::json!(max_results));
            }
            with_json_body(
                "POST",
                format!("{}/search/jql", jira_base(&cloud_id)),
                Value::Object(body),
            )
        }
        "jira_issue_get" => {
            let cloud_id = req_text(args, "cloud_id")?;
            let issue_key = req_text(args, "issue_key")?;
            Ok(get(format!(
                "{}/issue/{issue_key}?fields=summary,status,assignee,issuetype,priority",
                jira_base(&cloud_id)
            )))
        }
        "jira_issue_create" => {
            require_confirm(args, "jira_issue_create")?;
            let cloud_id = req_text(args, "cloud_id")?;
            let project_key = req_text(args, "project_key")?;
            let issue_type = req_text(args, "issue_type")?;
            let summary = req_text(args, "summary")?;
            let mut fields = Map::new();
            fields.insert(
                "project".to_string(),
                serde_json::json!({ "key": project_key }),
            );
            fields.insert(
                "issuetype".to_string(),
                serde_json::json!({ "name": issue_type }),
            );
            fields.insert("summary".to_string(), Value::String(summary));
            if let Some(description) = opt_text(args, "description") {
                fields.insert("description".to_string(), adf_doc(&description));
            }
            with_json_body(
                "POST",
                format!("{}/issue", jira_base(&cloud_id)),
                serde_json::json!({ "fields": Value::Object(fields) }),
            )
        }
        "jira_issue_comment" => {
            require_confirm(args, "jira_issue_comment")?;
            let cloud_id = req_text(args, "cloud_id")?;
            let issue_key = req_text(args, "issue_key")?;
            let text = req_text(args, "body")?;
            with_json_body(
                "POST",
                format!("{}/issue/{issue_key}/comment", jira_base(&cloud_id)),
                serde_json::json!({ "body": adf_doc(&text) }),
            )
        }
        "jira_issue_transition" => {
            require_confirm(args, "jira_issue_transition")?;
            let cloud_id = req_text(args, "cloud_id")?;
            let issue_key = req_text(args, "issue_key")?;
            let transition_id = req_text(args, "transition_id")?;
            with_json_body(
                "POST",
                format!("{}/issue/{issue_key}/transitions", jira_base(&cloud_id)),
                serde_json::json!({ "transition": { "id": transition_id } }),
            )
        }
        "confluence_search" => {
            let cloud_id = req_text(args, "cloud_id")?;
            let cql = req_text(args, "cql")?;
            let mut url = format!(
                "{}/search?cql={}",
                confluence_v1_base(&cloud_id),
                percent_encode(&cql)
            );
            if let Some(limit) = opt_integer(args, "limit") {
                url.push_str(&format!("&limit={limit}"));
            }
            Ok(get(url))
        }
        "confluence_page_get" => {
            let cloud_id = req_text(args, "cloud_id")?;
            let page_id = req_text(args, "page_id")?;
            Ok(get(format!(
                "{}/pages/{page_id}?body-format=storage",
                confluence_v2_base(&cloud_id)
            )))
        }
        "confluence_page_create" => {
            require_confirm(args, "confluence_page_create")?;
            let cloud_id = req_text(args, "cloud_id")?;
            let space_id = req_text(args, "space_id")?;
            let title = req_text(args, "title")?;
            let body_text = req_text(args, "body")?;
            let mut body = Map::new();
            body.insert("spaceId".to_string(), Value::String(space_id));
            body.insert("status".to_string(), Value::String("current".to_string()));
            body.insert("title".to_string(), Value::String(title));
            body.insert(
                "body".to_string(),
                serde_json::json!({ "representation": "storage", "value": body_text }),
            );
            if let Some(parent_id) = opt_text(args, "parent_id") {
                body.insert("parentId".to_string(), Value::String(parent_id));
            }
            with_json_body(
                "POST",
                format!("{}/pages", confluence_v2_base(&cloud_id)),
                Value::Object(body),
            )
        }
        "confluence_page_update" => {
            require_confirm(args, "confluence_page_update")?;
            let cloud_id = req_text(args, "cloud_id")?;
            let page_id = req_text(args, "page_id")?;
            let title = req_text(args, "title")?;
            let body_text = req_text(args, "body")?;
            let version = req_integer(args, "version")?;
            let mut body = Map::new();
            body.insert("id".to_string(), Value::String(page_id.clone()));
            body.insert("status".to_string(), Value::String("current".to_string()));
            body.insert("title".to_string(), Value::String(title));
            body.insert(
                "body".to_string(),
                serde_json::json!({ "representation": "storage", "value": body_text }),
            );
            body.insert(
                "version".to_string(),
                serde_json::json!({ "number": version }),
            );
            with_json_body(
                "PUT",
                format!("{}/pages/{page_id}", confluence_v2_base(&cloud_id)),
                Value::Object(body),
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
/// connected" rather than raised as an error.
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
                let sites: Vec<Value> = value
                    .as_array()
                    .map(|items| items.iter().map(summarize_site).collect())
                    .unwrap_or_default();
                let result = serde_json::json!({ "connected": true, "sites": sites });
                Ok(text_value(&result))
            }
            401 | 403 => Ok(not_connected()),
            other => Err(status_error(other, body)),
        };
    }

    if !(200..300).contains(&status) {
        return Err(match status {
            404 => ToolError::NotFound,
            other => status_error(other, body),
        });
    }

    match tool {
        "jira_search" => {
            let value = parse_json(body)?;
            let issues: Vec<Value> = value
                .get("issues")
                .and_then(Value::as_array)
                .map(|items| items.iter().map(summarize_jira_issue).collect())
                .unwrap_or_default();
            let mut result = serde_json::json!({ "issues": issues });
            if let Some(token) = value.get("nextPageToken") {
                result["next_page_token"] = token.clone();
            }
            Ok(text_value(&result))
        }
        "jira_issue_get" => Ok(text_value(&summarize_jira_issue(&parse_json(body)?))),
        "jira_issue_create" => {
            let value = parse_json(body)?;
            let result = serde_json::json!({
                "id": value.get("id"),
                "key": value.get("key"),
                "url": value.get("self"),
            });
            Ok(text_value(&result))
        }
        "jira_issue_comment" => {
            let value = parse_json(body)?;
            let result = serde_json::json!({
                "id": value.get("id"),
                "url": value.get("self"),
            });
            Ok(text_value(&result))
        }
        "jira_issue_transition" => {
            // Jira returns `204 No Content` on a successful transition (no
            // body to parse). Tolerate a non-empty body defensively in case a
            // caller's proxy attaches one, but never require it.
            Ok(text_value(&serde_json::json!({ "transitioned": true })))
        }
        "confluence_search" => {
            let value = parse_json(body)?;
            let results: Vec<Value> = value
                .get("results")
                .and_then(Value::as_array)
                .map(|items| items.iter().map(summarize_confluence_result).collect())
                .unwrap_or_default();
            Ok(text_value(&serde_json::json!({ "results": results })))
        }
        "confluence_page_get" | "confluence_page_create" | "confluence_page_update" => {
            Ok(text_value(&summarize_confluence_page(&parse_json(body)?)))
        }
        other => Err(ToolError::InvalidCall(format!("unknown tool: {other}"))),
    }
}

/// Parse a JSON body, mapping a parse failure to [`ToolError::Failed`].
fn parse_json(body: &[u8]) -> Result<Value, ToolError> {
    serde_json::from_slice(body)
        .map_err(|e| ToolError::Failed(format!("Atlassian response is not JSON: {e}")))
}

/// Wrap a JSON value as a single-element connector text value list.
fn text_value(value: &Value) -> Vec<ToolValueOut> {
    vec![ToolValueOut::Text(value.to_string())]
}

/// A generic non-2xx failure, carrying the status and Atlassian's own error
/// message when the body is JSON (`message`, or the first of `errorMessages`).
fn status_error(status: u16, body: &[u8]) -> ToolError {
    match atlassian_message(body) {
        Some(message) if !message.is_empty() => {
            ToolError::Failed(format!("Atlassian API error: HTTP {status}: {message}"))
        }
        _ => ToolError::Failed(format!("Atlassian API error: HTTP {status}")),
    }
}

/// The error message from an Atlassian JSON error body, if any. Jira errors
/// carry `errorMessages: [string]`; Confluence errors carry `message`.
fn atlassian_message(body: &[u8]) -> Option<String> {
    let value: Value = serde_json::from_slice(body).ok()?;
    if let Some(message) = value.get("message").and_then(Value::as_str) {
        return Some(message.to_string());
    }
    value
        .get("errorMessages")
        .and_then(Value::as_array)
        .and_then(|items| items.first())
        .and_then(Value::as_str)
        .map(str::to_string)
}

fn summarize_site(value: &Value) -> Value {
    serde_json::json!({
        "cloud_id": value.get("id"),
        "name": value.get("name"),
        "url": value.get("url"),
    })
}

fn summarize_jira_issue(value: &Value) -> Value {
    let fields = value.get("fields");
    serde_json::json!({
        "key": value.get("key"),
        "id": value.get("id"),
        "summary": fields.and_then(|f| f.get("summary")),
        "status": fields.and_then(|f| f.get("status")).and_then(|s| s.get("name")),
        "assignee": fields
            .and_then(|f| f.get("assignee"))
            .and_then(|a| a.get("displayName")),
    })
}

fn summarize_confluence_result(value: &Value) -> Value {
    let content = value.get("content");
    serde_json::json!({
        "id": content.and_then(|c| c.get("id")),
        "type": content.and_then(|c| c.get("type")),
        "title": value.get("title"),
        "excerpt": value.get("excerpt"),
        "url": value.get("url"),
    })
}

fn summarize_confluence_page(value: &Value) -> Value {
    serde_json::json!({
        "id": value.get("id"),
        "title": value.get("title"),
        "status": value.get("status"),
        "space_id": value.get("spaceId"),
        "version": value.get("version").and_then(|v| v.get("number")),
    })
}

/// The connector value list a "not connected" `auth_status` reports. Shared by
/// the `401`/`403` response path and the guest's `denied`/`expired` OAuth
/// path, so a missing/expired token never surfaces as a tool error.
pub fn not_connected() -> Vec<ToolValueOut> {
    let value = serde_json::json!({
        "connected": false,
        "message": "Atlassian is not connected. Connect it via Cockpit's Plugins screen."
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

    const CLOUD: &str = "1324a887-45db-1bf4-1e99-ef0ff456d421";

    /// Every URL a planned request targets must resolve to `api.atlassian.com`
    /// — the only host the manifest authorizes for actual tool traffic.
    fn assert_confined_to_api_atlassian_com(req: &PlannedRequest) {
        assert!(
            req.url.starts_with("https://api.atlassian.com/"),
            "planned request left api.atlassian.com: {}",
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
                "jira_search",
                "jira_issue_get",
                "jira_issue_create",
                "jira_issue_comment",
                "jira_issue_transition",
                "confluence_search",
                "confluence_page_get",
                "confluence_page_create",
                "confluence_page_update",
            ]
        );
    }

    #[test]
    fn every_mutating_tool_declares_a_required_confirm_boolean() {
        let defs = tool_definitions();
        for name in [
            "jira_issue_create",
            "jira_issue_comment",
            "jira_issue_transition",
            "confluence_page_create",
            "confluence_page_update",
        ] {
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
            "jira_search",
            "jira_issue_get",
            "confluence_search",
            "confluence_page_get",
        ] {
            let def = defs.iter().find(|d| d.name == name).unwrap();
            assert!(
                def.parameters.iter().all(|p| p.name != "confirm"),
                "{name} must not gate on confirm"
            );
        }
    }

    #[test]
    fn every_jira_and_confluence_tool_requires_cloud_id() {
        let defs = tool_definitions();
        for name in [
            "jira_search",
            "jira_issue_get",
            "jira_issue_create",
            "jira_issue_comment",
            "jira_issue_transition",
            "confluence_search",
            "confluence_page_get",
            "confluence_page_create",
            "confluence_page_update",
        ] {
            let def = defs.iter().find(|d| d.name == name).unwrap();
            let cloud_id = def
                .parameters
                .iter()
                .find(|p| p.name == "cloud_id")
                .unwrap_or_else(|| panic!("{name} must declare cloud_id"));
            assert!(cloud_id.required, "{name}.cloud_id must be required");
        }
        // auth_status is the one tool with no cloud_id — it is how a caller
        // discovers which cloud_ids exist in the first place.
        let auth_status = defs.iter().find(|d| d.name == "auth_status").unwrap();
        assert!(auth_status.parameters.iter().all(|p| p.name != "cloud_id"));
    }

    // ---------------- read-only planning ----------------

    #[test]
    fn auth_status_gets_accessible_resources() {
        let req = plan_request("auth_status", &[]).unwrap();
        assert_eq!(req.method, "GET");
        assert_eq!(
            req.url,
            "https://api.atlassian.com/oauth/token/accessible-resources"
        );
        assert!(req.body.is_none());
        assert_eq!(header(&req, "accept"), Some(ACCEPT));
        // The component never sets its own authorization; the host injects it.
        assert!(header(&req, "authorization").is_none());
        assert_confined_to_api_atlassian_com(&req);
    }

    #[test]
    fn jira_search_posts_jql_through_the_gateway() {
        let req = plan_request(
            "jira_search",
            &[
                t("cloud_id", CLOUD),
                t("jql", "project = OPS AND status = Open"),
            ],
        )
        .unwrap();
        assert_eq!(req.method, "POST");
        assert_eq!(
            req.url,
            format!("https://api.atlassian.com/ex/jira/{CLOUD}/rest/api/3/search/jql")
        );
        let body = body_json(&req);
        assert_eq!(body["jql"], "project = OPS AND status = Open");
        assert_eq!(body["fields"], json!(["summary", "status", "assignee"]));
        assert!(body.get("maxResults").is_none());
        assert_confined_to_api_atlassian_com(&req);
        assert!(header(&req, "authorization").is_none());
    }

    #[test]
    fn jira_search_forwards_max_results() {
        let req = plan_request(
            "jira_search",
            &[
                t("cloud_id", CLOUD),
                t("jql", "project = OPS"),
                i("max_results", 25),
            ],
        )
        .unwrap();
        assert_eq!(body_json(&req)["maxResults"], 25);
    }

    #[test]
    fn jira_search_requires_cloud_id_and_jql() {
        let err = plan_request("jira_search", &[t("jql", "project = OPS")]).unwrap_err();
        assert!(matches!(err, ToolError::InvalidCall(_)));
        let err = plan_request("jira_search", &[t("cloud_id", CLOUD)]).unwrap_err();
        assert!(matches!(err, ToolError::InvalidCall(_)));
    }

    #[test]
    fn jira_issue_get_targets_the_issue_endpoint_with_a_field_list() {
        let req = plan_request(
            "jira_issue_get",
            &[t("cloud_id", CLOUD), t("issue_key", "OPS-42")],
        )
        .unwrap();
        assert_eq!(req.method, "GET");
        assert_eq!(
            req.url,
            format!(
                "https://api.atlassian.com/ex/jira/{CLOUD}/rest/api/3/issue/OPS-42?fields=summary,status,assignee,issuetype,priority"
            )
        );
        assert_confined_to_api_atlassian_com(&req);
    }

    #[test]
    fn confluence_search_encodes_the_cql_query() {
        let req = plan_request(
            "confluence_search",
            &[
                t("cloud_id", CLOUD),
                t("cql", "space = TEST AND type = page"),
            ],
        )
        .unwrap();
        assert_eq!(req.method, "GET");
        assert_eq!(
            req.url,
            format!(
                "https://api.atlassian.com/ex/confluence/{CLOUD}/wiki/rest/api/search?cql=space%20%3D%20TEST%20AND%20type%20%3D%20page"
            )
        );
        assert_confined_to_api_atlassian_com(&req);
    }

    #[test]
    fn confluence_search_forwards_limit() {
        let req = plan_request(
            "confluence_search",
            &[t("cloud_id", CLOUD), t("cql", "type=page"), i("limit", 10)],
        )
        .unwrap();
        assert!(req.url.ends_with("&limit=10"));
    }

    #[test]
    fn confluence_page_get_uses_the_v2_pages_endpoint() {
        let req = plan_request(
            "confluence_page_get",
            &[t("cloud_id", CLOUD), t("page_id", "3604482")],
        )
        .unwrap();
        assert_eq!(req.method, "GET");
        assert_eq!(
            req.url,
            format!(
                "https://api.atlassian.com/ex/confluence/{CLOUD}/wiki/api/v2/pages/3604482?body-format=storage"
            )
        );
        assert_confined_to_api_atlassian_com(&req);
    }

    // ---------------- the confirmation gate ----------------

    /// The central "an unapproved request is not sent" guarantee: every
    /// mutating tool, called WITHOUT confirm, must yield ConfirmationRequired
    /// and never a PlannedRequest.
    #[test]
    fn every_mutation_without_confirm_is_refused_before_any_request() {
        let cases: Vec<(&str, Vec<Arg>)> = vec![
            (
                "jira_issue_create",
                vec![
                    t("cloud_id", CLOUD),
                    t("project_key", "OPS"),
                    t("issue_type", "Bug"),
                    t("summary", "Broken"),
                ],
            ),
            (
                "jira_issue_comment",
                vec![
                    t("cloud_id", CLOUD),
                    t("issue_key", "OPS-1"),
                    t("body", "hi"),
                ],
            ),
            (
                "jira_issue_transition",
                vec![
                    t("cloud_id", CLOUD),
                    t("issue_key", "OPS-1"),
                    t("transition_id", "31"),
                ],
            ),
            (
                "confluence_page_create",
                vec![
                    t("cloud_id", CLOUD),
                    t("space_id", "2719747"),
                    t("title", "New page"),
                    t("body", "<p>hi</p>"),
                ],
            ),
            (
                "confluence_page_update",
                vec![
                    t("cloud_id", CLOUD),
                    t("page_id", "3604482"),
                    t("title", "Updated"),
                    t("body", "<p>hi</p>"),
                    i("version", 2),
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
            "jira_issue_comment",
            &[
                t("cloud_id", CLOUD),
                t("issue_key", "OPS-1"),
                t("body", "hi"),
                t("confirm", "true"),
            ],
        )
        .unwrap();
        assert_eq!(req.method, "POST");
    }

    // ---------------- mutating planning (confirmed) ----------------

    #[test]
    fn jira_issue_create_posts_project_type_and_summary() {
        let req = plan_request(
            "jira_issue_create",
            &[
                t("cloud_id", CLOUD),
                t("project_key", "OPS"),
                t("issue_type", "Bug"),
                t("summary", "Broken"),
                t("description", "It broke"),
                b("confirm", true),
            ],
        )
        .unwrap();
        assert_eq!(req.method, "POST");
        assert_eq!(
            req.url,
            format!("https://api.atlassian.com/ex/jira/{CLOUD}/rest/api/3/issue")
        );
        let body = body_json(&req);
        assert_eq!(body["fields"]["project"]["key"], "OPS");
        assert_eq!(body["fields"]["issuetype"]["name"], "Bug");
        assert_eq!(body["fields"]["summary"], "Broken");
        // description is Atlassian Document Format, not a bare string.
        assert_eq!(body["fields"]["description"]["type"], "doc");
        assert_eq!(
            body["fields"]["description"]["content"][0]["content"][0]["text"],
            "It broke"
        );
        assert_confined_to_api_atlassian_com(&req);
        assert!(header(&req, "authorization").is_none());
    }

    #[test]
    fn jira_issue_create_requires_project_key_type_and_summary() {
        let err = plan_request(
            "jira_issue_create",
            &[t("cloud_id", CLOUD), b("confirm", true)],
        )
        .unwrap_err();
        assert!(matches!(err, ToolError::InvalidCall(_)));
    }

    #[test]
    fn jira_issue_comment_posts_adf_body_to_the_comment_endpoint() {
        let req = plan_request(
            "jira_issue_comment",
            &[
                t("cloud_id", CLOUD),
                t("issue_key", "OPS-42"),
                t("body", "thanks"),
                b("confirm", true),
            ],
        )
        .unwrap();
        assert_eq!(
            req.url,
            format!("https://api.atlassian.com/ex/jira/{CLOUD}/rest/api/3/issue/OPS-42/comment")
        );
        let body = body_json(&req);
        assert_eq!(body["body"]["type"], "doc");
        assert_eq!(body["body"]["content"][0]["content"][0]["text"], "thanks");
    }

    #[test]
    fn jira_issue_transition_posts_the_transition_id() {
        let req = plan_request(
            "jira_issue_transition",
            &[
                t("cloud_id", CLOUD),
                t("issue_key", "OPS-42"),
                t("transition_id", "31"),
                b("confirm", true),
            ],
        )
        .unwrap();
        assert_eq!(
            req.url,
            format!(
                "https://api.atlassian.com/ex/jira/{CLOUD}/rest/api/3/issue/OPS-42/transitions"
            )
        );
        assert_eq!(body_json(&req)["transition"]["id"], "31");
    }

    #[test]
    fn confluence_page_create_posts_storage_body_to_the_space() {
        let req = plan_request(
            "confluence_page_create",
            &[
                t("cloud_id", CLOUD),
                t("space_id", "2719747"),
                t("title", "New page"),
                t("body", "<p>hi</p>"),
                b("confirm", true),
            ],
        )
        .unwrap();
        assert_eq!(req.method, "POST");
        assert_eq!(
            req.url,
            format!("https://api.atlassian.com/ex/confluence/{CLOUD}/wiki/api/v2/pages")
        );
        let body = body_json(&req);
        assert_eq!(body["spaceId"], "2719747");
        assert_eq!(body["title"], "New page");
        assert_eq!(body["body"]["representation"], "storage");
        assert_eq!(body["body"]["value"], "<p>hi</p>");
        assert!(body.get("parentId").is_none());
    }

    #[test]
    fn confluence_page_create_forwards_an_optional_parent_id() {
        let req = plan_request(
            "confluence_page_create",
            &[
                t("cloud_id", CLOUD),
                t("space_id", "2719747"),
                t("title", "Child"),
                t("body", "<p>hi</p>"),
                t("parent_id", "3604482"),
                b("confirm", true),
            ],
        )
        .unwrap();
        assert_eq!(body_json(&req)["parentId"], "3604482");
    }

    #[test]
    fn confluence_page_update_puts_the_new_version_number() {
        let req = plan_request(
            "confluence_page_update",
            &[
                t("cloud_id", CLOUD),
                t("page_id", "3604482"),
                t("title", "Updated"),
                t("body", "<p>v2</p>"),
                i("version", 5),
                b("confirm", true),
            ],
        )
        .unwrap();
        assert_eq!(req.method, "PUT");
        assert_eq!(
            req.url,
            format!("https://api.atlassian.com/ex/confluence/{CLOUD}/wiki/api/v2/pages/3604482")
        );
        let body = body_json(&req);
        assert_eq!(body["id"], "3604482");
        assert_eq!(body["version"]["number"], 5);
    }

    #[test]
    fn unknown_tool_is_invalid_call() {
        let err = plan_request("delete_everything", &[]).unwrap_err();
        assert!(matches!(err, ToolError::InvalidCall(_)));
    }

    // ---------------- response parsing ----------------

    #[test]
    fn auth_status_200_reports_connected_sites() {
        let body = br#"[
            {"id":"1324a887-45db-1bf4-1e99-ef0ff456d421","name":"Site name","url":"https://your-domain.atlassian.net","scopes":["write:jira-work"]}
        ]"#;
        let values = parse_response("auth_status", 200, body).unwrap();
        let ToolValueOut::Text(text) = &values[0] else {
            panic!("expected a text value");
        };
        let parsed: Value = serde_json::from_str(text).unwrap();
        assert_eq!(parsed["connected"], true);
        assert_eq!(parsed["sites"][0]["cloud_id"], CLOUD);
        assert_eq!(
            parsed["sites"][0]["url"],
            "https://your-domain.atlassian.net"
        );
    }

    #[test]
    fn auth_status_401_reports_not_connected_without_erroring() {
        let values = parse_response("auth_status", 401, br#"{"message":"unauthorized"}"#).unwrap();
        let ToolValueOut::Text(text) = &values[0] else {
            panic!("expected a text value");
        };
        let parsed: Value = serde_json::from_str(text).unwrap();
        assert_eq!(parsed["connected"], false);
    }

    #[test]
    fn jira_search_200_summarizes_each_issue() {
        let body = br#"{
            "issues":[
                {"id":"10068","key":"OPS-1","fields":{"summary":"Broken","status":{"name":"Open"},"assignee":{"displayName":"Ana"}}}
            ],
            "nextPageToken":"CAEaAggD"
        }"#;
        let values = parse_response("jira_search", 200, body).unwrap();
        let ToolValueOut::Text(text) = &values[0] else {
            panic!("expected a text value");
        };
        let parsed: Value = serde_json::from_str(text).unwrap();
        assert_eq!(parsed["issues"][0]["key"], "OPS-1");
        assert_eq!(parsed["issues"][0]["summary"], "Broken");
        assert_eq!(parsed["issues"][0]["status"], "Open");
        assert_eq!(parsed["issues"][0]["assignee"], "Ana");
        assert_eq!(parsed["next_page_token"], "CAEaAggD");
    }

    #[test]
    fn jira_issue_get_200_summarizes_the_issue() {
        let body = br#"{
            "id":"10068","key":"OPS-1",
            "fields":{"summary":"Broken","status":{"name":"Open"},"assignee":null}
        }"#;
        let values = parse_response("jira_issue_get", 200, body).unwrap();
        let ToolValueOut::Text(text) = &values[0] else {
            panic!("expected a text value");
        };
        let parsed: Value = serde_json::from_str(text).unwrap();
        assert_eq!(parsed["key"], "OPS-1");
        assert_eq!(parsed["summary"], "Broken");
        assert_eq!(parsed["assignee"], Value::Null);
    }

    #[test]
    fn jira_issue_get_404_is_not_found() {
        let err = parse_response("jira_issue_get", 404, b"{}").unwrap_err();
        assert!(matches!(err, ToolError::NotFound));
    }

    #[test]
    fn jira_issue_create_201_returns_id_key_and_url() {
        let body =
            br#"{"id":"10000","key":"OPS-99","self":"https://api.atlassian.com/ex/jira/x/rest/api/3/issue/10000"}"#;
        let values = parse_response("jira_issue_create", 201, body).unwrap();
        let ToolValueOut::Text(text) = &values[0] else {
            panic!("expected a text value");
        };
        let parsed: Value = serde_json::from_str(text).unwrap();
        assert_eq!(parsed["key"], "OPS-99");
        assert_eq!(parsed["id"], "10000");
    }

    #[test]
    fn jira_issue_transition_204_reports_success_with_no_body() {
        let values = parse_response("jira_issue_transition", 204, b"").unwrap();
        let ToolValueOut::Text(text) = &values[0] else {
            panic!("expected a text value");
        };
        let parsed: Value = serde_json::from_str(text).unwrap();
        assert_eq!(parsed["transitioned"], true);
    }

    #[test]
    fn confluence_search_200_summarizes_each_result() {
        let body = br#"{
            "results":[
                {"content":{"id":"3965071","type":"page"},"title":"Test Space Home","excerpt":"hi","url":"/spaces/TST/pages/3965071"}
            ]
        }"#;
        let values = parse_response("confluence_search", 200, body).unwrap();
        let ToolValueOut::Text(text) = &values[0] else {
            panic!("expected a text value");
        };
        let parsed: Value = serde_json::from_str(text).unwrap();
        assert_eq!(parsed["results"][0]["id"], "3965071");
        assert_eq!(parsed["results"][0]["type"], "page");
        assert_eq!(parsed["results"][0]["title"], "Test Space Home");
    }

    #[test]
    fn confluence_page_get_200_summarizes_the_page() {
        let body = br#"{
            "id":"3604482","status":"current","title":"new page","spaceId":"2719747",
            "version":{"number":3}
        }"#;
        let values = parse_response("confluence_page_get", 200, body).unwrap();
        let ToolValueOut::Text(text) = &values[0] else {
            panic!("expected a text value");
        };
        let parsed: Value = serde_json::from_str(text).unwrap();
        assert_eq!(parsed["id"], "3604482");
        assert_eq!(parsed["title"], "new page");
        assert_eq!(parsed["space_id"], "2719747");
        assert_eq!(parsed["version"], 3);
    }

    #[test]
    fn confluence_page_update_200_summarizes_the_new_version() {
        let body = br#"{"id":"3604482","status":"current","title":"Updated","spaceId":"2719747","version":{"number":4}}"#;
        let values = parse_response("confluence_page_update", 200, body).unwrap();
        let ToolValueOut::Text(text) = &values[0] else {
            panic!("expected a text value");
        };
        let parsed: Value = serde_json::from_str(text).unwrap();
        assert_eq!(parsed["version"], 4);
    }

    #[test]
    fn a_500_is_a_failure_carrying_the_status() {
        let err = parse_response("jira_issue_get", 500, b"boom").unwrap_err();
        match err {
            ToolError::Failed(message) => assert!(message.contains("500")),
            other => panic!("expected Failed, got {other:?}"),
        }
    }

    #[test]
    fn a_jira_error_message_is_surfaced() {
        let err = parse_response(
            "jira_issue_get",
            400,
            br#"{"errorMessages":["The JQL query is invalid"]}"#,
        )
        .unwrap_err();
        match err {
            ToolError::Failed(message) => assert!(message.contains("The JQL query is invalid")),
            other => panic!("expected Failed, got {other:?}"),
        }
    }

    #[test]
    fn a_confluence_error_message_is_surfaced() {
        let err = parse_response(
            "confluence_page_get",
            403,
            br#"{"message":"No permission to view this page"}"#,
        )
        .unwrap_err();
        match err {
            ToolError::Failed(message) => {
                assert!(message.contains("No permission to view this page"))
            }
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
