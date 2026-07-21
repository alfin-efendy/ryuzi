//! Pure, host-free GitHub connector logic. Every function here is
//! deterministic over its inputs (no network, no clock, no storage), so the
//! whole module is covered by native `cargo test`. The wasm `guest` glue
//! supplies the live effect — one `oauth.authorized-request("github", ..)`
//! per planned request — and maps these plain types to/from WIT.
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

/// GitHub REST/GraphQL API origin. Every planned request targets this host,
/// which the manifest's `api.github.com` network entry authorizes.
pub const API_BASE: &str = "https://api.github.com";

/// The OAuth profile id the guest passes to `authorized-request` — matches the
/// `[[oauth]] id` in `ryuzi-plugin.toml`.
pub const OAUTH_PROFILE: &str = "github";

/// GitHub requires every request to carry a `User-Agent`; the host does not add
/// one (it only strips `host`/`content-length` and manages `authorization`), so
/// the component must supply it or GitHub answers `403`.
pub const USER_AGENT: &str = "ryuzi-github-connector";

/// Pinned REST API version header value (GitHub `2022-11-28` media type era).
pub const API_VERSION: &str = "2022-11-28";

/// The `Accept` media type for GitHub's JSON REST API.
pub const ACCEPT: &str = "application/vnd.github+json";

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
            "'{tool}' is a mutating GitHub operation; re-invoke with confirm=true to proceed"
        )))
    }
}

/// `true` if a GraphQL document is a mutation, i.e. its trimmed text begins with
/// the `mutation` keyword (per the reconciliation rule: gate anything whose
/// trimmed text starts with `mutation`).
pub fn is_graphql_mutation(query: &str) -> bool {
    let trimmed = query.trim_start();
    match trimmed.strip_prefix("mutation") {
        // Bare `mutation` or `mutation` followed by a non-identifier boundary
        // (name, `(`, or `{`) — not an identifier like `mutations`.
        Some(rest) => rest
            .chars()
            .next()
            .map(|c| !(c.is_alphanumeric() || c == '_'))
            .unwrap_or(true),
        None => false,
    }
}

// ---------------------------------------------------------------------------
// request/response builders
// ---------------------------------------------------------------------------

/// Standard GitHub request headers. `authorization` is deliberately absent —
/// the host injects the bearer for the selected profile.
fn base_headers(with_body: bool) -> Vec<(String, String)> {
    let mut headers = vec![
        ("user-agent".to_string(), USER_AGENT.to_string()),
        ("accept".to_string(), ACCEPT.to_string()),
        ("x-github-api-version".to_string(), API_VERSION.to_string()),
    ];
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

fn with_json_body(method: &str, url: String, body: Value) -> PlannedRequest {
    PlannedRequest {
        method: method.to_string(),
        url,
        headers: base_headers(true),
        body: Some(serde_json::to_vec(&body).expect("json body always serializes")),
    }
}

/// One valid value out of a small set, or [`ToolError::InvalidCall`]. Used for
/// the `state`/`event`/`method` enums GitHub accepts.
fn one_of(value: &str, allowed: &[&str], field: &str) -> Result<String, ToolError> {
    if allowed.contains(&value) {
        Ok(value.to_string())
    } else {
        Err(ToolError::InvalidCall(format!(
            "'{field}' must be one of {allowed:?}, got {value:?}"
        )))
    }
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
    let confirm = || param("confirm", "boolean", true);
    vec![
        def(
            "auth_status",
            "Report whether GitHub is connected, and the authenticated login/name when it is.",
            vec![],
        ),
        def(
            "repo_get",
            "Get metadata for a repository (owner/repo).",
            vec![param("owner", "string", true), param("repo", "string", true)],
        ),
        def(
            "repo_list",
            "List repositories for the authenticated user, or for `user` when given.",
            vec![param("user", "string", false)],
        ),
        def(
            "issue_list",
            "List issues in a repository. Optional `state` = open | closed | all.",
            vec![
                param("owner", "string", true),
                param("repo", "string", true),
                param("state", "string", false),
            ],
        ),
        def(
            "pr_list",
            "List pull requests in a repository. Optional `state` = open | closed | all.",
            vec![
                param("owner", "string", true),
                param("repo", "string", true),
                param("state", "string", false),
            ],
        ),
        def(
            "rest_get",
            "Arbitrary authenticated GET against api.github.com; `path` is the API path (e.g. /rate_limit).",
            vec![param("path", "string", true)],
        ),
        def(
            "graphql",
            "Run a GitHub GraphQL query. A mutation (query starting with `mutation`) requires confirm=true.",
            vec![
                param("query", "string", true),
                param("variables_json", "string", false),
                param("confirm", "boolean", false),
            ],
        ),
        def(
            "issue_create",
            "Create an issue. Mutating: requires confirm=true.",
            vec![
                param("owner", "string", true),
                param("repo", "string", true),
                param("title", "string", true),
                param("body", "string", false),
                confirm(),
            ],
        ),
        def(
            "issue_comment",
            "Comment on an issue. Mutating: requires confirm=true.",
            vec![
                param("owner", "string", true),
                param("repo", "string", true),
                param("issue_number", "integer", true),
                param("body", "string", true),
                confirm(),
            ],
        ),
        def(
            "pr_create",
            "Open a pull request. Mutating: requires confirm=true.",
            vec![
                param("owner", "string", true),
                param("repo", "string", true),
                param("title", "string", true),
                param("head", "string", true),
                param("base", "string", true),
                param("body", "string", false),
                confirm(),
            ],
        ),
        def(
            "pr_review",
            "Submit a pull-request review. `event` = APPROVE | REQUEST_CHANGES | COMMENT. Mutating: requires confirm=true.",
            vec![
                param("owner", "string", true),
                param("repo", "string", true),
                param("pr_number", "integer", true),
                param("event", "string", true),
                param("body", "string", false),
                confirm(),
            ],
        ),
        def(
            "pr_merge",
            "Merge a pull request. `method` = merge | squash | rebase. Mutating: requires confirm=true.",
            vec![
                param("owner", "string", true),
                param("repo", "string", true),
                param("pr_number", "integer", true),
                param("method", "string", false),
                confirm(),
            ],
        ),
    ]
}

// ---------------------------------------------------------------------------
// planning
// ---------------------------------------------------------------------------

/// The list endpoint (`issues`/`pulls`) URL for `owner`/`repo`, with an
/// optional validated `state` query.
fn list_url(args: &[Arg], kind: &str) -> Result<String, ToolError> {
    let owner = req_text(args, "owner")?;
    let repo = req_text(args, "repo")?;
    let mut url = format!("{API_BASE}/repos/{owner}/{repo}/{kind}");
    if let Some(state) = opt_text(args, "state") {
        let state = one_of(&state, &["open", "closed", "all"], "state")?;
        url.push_str(&format!("?state={state}"));
    }
    Ok(url)
}

/// Build the HTTP request for `tool` from its `args`, or an error. A mutating
/// tool without `confirm=true` returns [`ToolError::ConfirmationRequired`] and
/// builds NO request.
pub fn plan_request(tool: &str, args: &[Arg]) -> Result<PlannedRequest, ToolError> {
    match tool {
        "auth_status" => Ok(get(format!("{API_BASE}/user"))),
        "repo_get" => {
            let owner = req_text(args, "owner")?;
            let repo = req_text(args, "repo")?;
            Ok(get(format!("{API_BASE}/repos/{owner}/{repo}")))
        }
        "repo_list" => {
            let url = match opt_text(args, "user") {
                Some(user) => format!("{API_BASE}/users/{user}/repos"),
                None => format!("{API_BASE}/user/repos"),
            };
            Ok(get(url))
        }
        "issue_list" => Ok(get(list_url(args, "issues")?)),
        "pr_list" => Ok(get(list_url(args, "pulls")?)),
        "rest_get" => {
            let path = req_text(args, "path")?;
            let path = path.trim_start_matches('/');
            Ok(get(format!("{API_BASE}/{path}")))
        }
        "graphql" => {
            let query = req_text(args, "query")?;
            // A mutation must be confirmed; a plain query need not be.
            if is_graphql_mutation(&query) {
                require_confirm(args, "graphql")?;
            }
            let mut body = Map::new();
            body.insert("query".to_string(), Value::String(query));
            if let Some(vars) = opt_text(args, "variables_json") {
                let parsed: Value = serde_json::from_str(&vars).map_err(|e| {
                    ToolError::InvalidCall(format!("variables_json is not valid JSON: {e}"))
                })?;
                body.insert("variables".to_string(), parsed);
            }
            Ok(with_json_body(
                "POST",
                format!("{API_BASE}/graphql"),
                Value::Object(body),
            ))
        }
        "issue_create" => {
            require_confirm(args, "issue_create")?;
            let owner = req_text(args, "owner")?;
            let repo = req_text(args, "repo")?;
            let title = req_text(args, "title")?;
            let mut body = Map::new();
            body.insert("title".to_string(), Value::String(title));
            if let Some(text) = opt_text(args, "body") {
                body.insert("body".to_string(), Value::String(text));
            }
            Ok(with_json_body(
                "POST",
                format!("{API_BASE}/repos/{owner}/{repo}/issues"),
                Value::Object(body),
            ))
        }
        "issue_comment" => {
            require_confirm(args, "issue_comment")?;
            let owner = req_text(args, "owner")?;
            let repo = req_text(args, "repo")?;
            let number = req_integer(args, "issue_number")?;
            let text = req_text(args, "body")?;
            Ok(with_json_body(
                "POST",
                format!("{API_BASE}/repos/{owner}/{repo}/issues/{number}/comments"),
                serde_json::json!({ "body": text }),
            ))
        }
        "pr_create" => {
            require_confirm(args, "pr_create")?;
            let owner = req_text(args, "owner")?;
            let repo = req_text(args, "repo")?;
            let title = req_text(args, "title")?;
            let head = req_text(args, "head")?;
            let base = req_text(args, "base")?;
            let mut body = Map::new();
            body.insert("title".to_string(), Value::String(title));
            body.insert("head".to_string(), Value::String(head));
            body.insert("base".to_string(), Value::String(base));
            if let Some(text) = opt_text(args, "body") {
                body.insert("body".to_string(), Value::String(text));
            }
            Ok(with_json_body(
                "POST",
                format!("{API_BASE}/repos/{owner}/{repo}/pulls"),
                Value::Object(body),
            ))
        }
        "pr_review" => {
            require_confirm(args, "pr_review")?;
            let owner = req_text(args, "owner")?;
            let repo = req_text(args, "repo")?;
            let number = req_integer(args, "pr_number")?;
            let event = one_of(
                &req_text(args, "event")?,
                &["APPROVE", "REQUEST_CHANGES", "COMMENT"],
                "event",
            )?;
            let mut body = Map::new();
            body.insert("event".to_string(), Value::String(event));
            if let Some(text) = opt_text(args, "body") {
                body.insert("body".to_string(), Value::String(text));
            }
            Ok(with_json_body(
                "POST",
                format!("{API_BASE}/repos/{owner}/{repo}/pulls/{number}/reviews"),
                Value::Object(body),
            ))
        }
        "pr_merge" => {
            require_confirm(args, "pr_merge")?;
            let owner = req_text(args, "owner")?;
            let repo = req_text(args, "repo")?;
            let number = req_integer(args, "pr_number")?;
            let method = match opt_text(args, "method") {
                Some(method) => one_of(&method, &["merge", "squash", "rebase"], "method")?,
                None => "merge".to_string(),
            };
            Ok(with_json_body(
                "PUT",
                format!("{API_BASE}/repos/{owner}/{repo}/pulls/{number}/merge"),
                serde_json::json!({ "merge_method": method }),
            ))
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
                let result = serde_json::json!({
                    "connected": true,
                    "login": value.get("login").and_then(Value::as_str).unwrap_or(""),
                    "name": value.get("name").and_then(Value::as_str).unwrap_or(""),
                });
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
        // Arbitrary GET / GraphQL pass their raw body through verbatim.
        "rest_get" | "graphql" => Ok(vec![ToolValueOut::Text(
            String::from_utf8_lossy(body).into_owned(),
        )]),
        "repo_get" => Ok(text_value(&summarize_repo(&parse_json(body)?))),
        "repo_list" => Ok(text_value(&summarize_list(
            &parse_json(body)?,
            summarize_repo,
        ))),
        "issue_list" => Ok(text_value(&summarize_list(
            &parse_json(body)?,
            summarize_issue,
        ))),
        "pr_list" => Ok(text_value(&summarize_list(
            &parse_json(body)?,
            summarize_pr,
        ))),
        "issue_create" | "issue_comment" | "pr_create" | "pr_review" => {
            Ok(text_value(&summarize_created(&parse_json(body)?)))
        }
        "pr_merge" => {
            let value = parse_json(body)?;
            let result = serde_json::json!({
                "merged": value.get("merged"),
                "sha": value.get("sha"),
                "message": value.get("message"),
            });
            Ok(text_value(&result))
        }
        other => Err(ToolError::InvalidCall(format!("unknown tool: {other}"))),
    }
}

/// Parse a JSON body, mapping a parse failure to [`ToolError::Failed`].
fn parse_json(body: &[u8]) -> Result<Value, ToolError> {
    serde_json::from_slice(body)
        .map_err(|e| ToolError::Failed(format!("GitHub response is not JSON: {e}")))
}

/// Wrap a JSON value as a single-element connector text value list.
fn text_value(value: &Value) -> Vec<ToolValueOut> {
    vec![ToolValueOut::Text(value.to_string())]
}

/// A generic non-2xx failure, carrying the status and GitHub's own `message`
/// field when the body is JSON.
fn status_error(status: u16, body: &[u8]) -> ToolError {
    match github_message(body) {
        Some(message) if !message.is_empty() => {
            ToolError::Failed(format!("GitHub API error: HTTP {status}: {message}"))
        }
        _ => ToolError::Failed(format!("GitHub API error: HTTP {status}")),
    }
}

/// The `message` string from a GitHub JSON error body, if any.
fn github_message(body: &[u8]) -> Option<String> {
    serde_json::from_slice::<Value>(body)
        .ok()?
        .get("message")?
        .as_str()
        .map(str::to_string)
}

/// Map an array response through `summarize`, yielding a JSON array (empty when
/// the body is not an array).
fn summarize_list(value: &Value, summarize: fn(&Value) -> Value) -> Value {
    let items = value
        .as_array()
        .map(|items| items.iter().map(summarize).collect())
        .unwrap_or_default();
    Value::Array(items)
}

fn summarize_repo(value: &Value) -> Value {
    serde_json::json!({
        "full_name": value.get("full_name"),
        "description": value.get("description"),
        "stars": value.get("stargazers_count"),
        "forks": value.get("forks_count"),
        "open_issues": value.get("open_issues_count"),
        "default_branch": value.get("default_branch"),
        "private": value.get("private"),
        "html_url": value.get("html_url"),
    })
}

fn summarize_issue(value: &Value) -> Value {
    serde_json::json!({
        "number": value.get("number"),
        "title": value.get("title"),
        "state": value.get("state"),
        "user": value.get("user").and_then(|user| user.get("login")),
        "html_url": value.get("html_url"),
    })
}

fn summarize_pr(value: &Value) -> Value {
    serde_json::json!({
        "number": value.get("number"),
        "title": value.get("title"),
        "state": value.get("state"),
        "user": value.get("user").and_then(|user| user.get("login")),
        "head": value.get("head").and_then(|head| head.get("ref")),
        "base": value.get("base").and_then(|base| base.get("ref")),
        "html_url": value.get("html_url"),
    })
}

/// Compact summary of a created/updated resource — only the identity fields
/// GitHub returns that are present and non-null.
fn summarize_created(value: &Value) -> Value {
    let mut out = Map::new();
    for key in ["number", "id", "state", "html_url"] {
        match value.get(key) {
            Some(field) if !field.is_null() => {
                out.insert(key.to_string(), field.clone());
            }
            _ => {}
        }
    }
    Value::Object(out)
}

/// The connector value list a "not connected" `auth_status` reports. Shared by
/// the `401` response path and the guest's `denied`/`expired` OAuth path, so a
/// missing/expired token never surfaces as a tool error.
pub fn not_connected() -> Vec<ToolValueOut> {
    let value = serde_json::json!({
        "connected": false,
        "message": "GitHub is not connected. Connect it via Cockpit's Plugins screen."
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

    // ---------------- tool catalogue ----------------

    #[test]
    fn tool_catalogue_has_the_expected_0_1_0_tools() {
        let names: Vec<String> = tool_definitions().into_iter().map(|d| d.name).collect();
        assert_eq!(
            names,
            vec![
                "auth_status",
                "repo_get",
                "repo_list",
                "issue_list",
                "pr_list",
                "rest_get",
                "graphql",
                "issue_create",
                "issue_comment",
                "pr_create",
                "pr_review",
                "pr_merge",
            ]
        );
    }

    #[test]
    fn every_mutating_tool_declares_a_required_confirm_boolean() {
        let defs = tool_definitions();
        for name in [
            "issue_create",
            "issue_comment",
            "pr_create",
            "pr_review",
            "pr_merge",
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
        for name in ["repo_get", "repo_list", "issue_list", "pr_list", "rest_get"] {
            let def = defs.iter().find(|d| d.name == name).unwrap();
            assert!(
                def.parameters.iter().all(|p| p.name != "confirm"),
                "{name} must not gate on confirm"
            );
        }
    }

    #[test]
    fn issue_and_pr_numbers_are_integer_typed() {
        let defs = tool_definitions();
        let param_type = |tool: &str, param: &str| -> String {
            defs.iter()
                .find(|d| d.name == tool)
                .unwrap()
                .parameters
                .iter()
                .find(|p| p.name == param)
                .unwrap()
                .value_type
                .clone()
        };
        assert_eq!(param_type("issue_comment", "issue_number"), "integer");
        assert_eq!(param_type("pr_review", "pr_number"), "integer");
        assert_eq!(param_type("pr_merge", "pr_number"), "integer");
    }

    // ---------------- read-only planning ----------------

    #[test]
    fn auth_status_gets_the_authenticated_user() {
        let req = plan_request("auth_status", &[]).unwrap();
        assert_eq!(req.method, "GET");
        assert_eq!(req.url, "https://api.github.com/user");
        assert!(req.body.is_none());
        assert_eq!(header(&req, "user-agent"), Some(USER_AGENT));
        assert_eq!(header(&req, "accept"), Some(ACCEPT));
        assert_eq!(header(&req, "x-github-api-version"), Some(API_VERSION));
        // The component never sets its own authorization; the host injects it.
        assert!(header(&req, "authorization").is_none());
    }

    #[test]
    fn repo_get_targets_the_repo_endpoint() {
        let req = plan_request("repo_get", &[t("owner", "octo"), t("repo", "hello")]).unwrap();
        assert_eq!(req.method, "GET");
        assert_eq!(req.url, "https://api.github.com/repos/octo/hello");
    }

    #[test]
    fn repo_get_requires_owner_and_repo() {
        let err = plan_request("repo_get", &[t("owner", "octo")]).unwrap_err();
        assert!(matches!(err, ToolError::InvalidCall(_)));
    }

    #[test]
    fn repo_list_defaults_to_the_authenticated_user() {
        let req = plan_request("repo_list", &[]).unwrap();
        assert_eq!(req.url, "https://api.github.com/user/repos");
    }

    #[test]
    fn repo_list_for_a_named_user_targets_that_user() {
        let req = plan_request("repo_list", &[t("user", "octocat")]).unwrap();
        assert_eq!(req.url, "https://api.github.com/users/octocat/repos");
    }

    #[test]
    fn issue_list_without_state_has_no_query() {
        let req = plan_request("issue_list", &[t("owner", "o"), t("repo", "r")]).unwrap();
        assert_eq!(req.url, "https://api.github.com/repos/o/r/issues");
    }

    #[test]
    fn issue_list_with_state_adds_the_query() {
        let req = plan_request(
            "issue_list",
            &[t("owner", "o"), t("repo", "r"), t("state", "all")],
        )
        .unwrap();
        assert_eq!(req.url, "https://api.github.com/repos/o/r/issues?state=all");
    }

    #[test]
    fn issue_list_rejects_an_unknown_state() {
        let err = plan_request(
            "issue_list",
            &[t("owner", "o"), t("repo", "r"), t("state", "bogus")],
        )
        .unwrap_err();
        assert!(matches!(err, ToolError::InvalidCall(_)));
    }

    #[test]
    fn pr_list_uses_the_pulls_endpoint_and_state() {
        let req = plan_request(
            "pr_list",
            &[t("owner", "o"), t("repo", "r"), t("state", "closed")],
        )
        .unwrap();
        assert_eq!(
            req.url,
            "https://api.github.com/repos/o/r/pulls?state=closed"
        );
    }

    #[test]
    fn rest_get_normalizes_a_leading_slash() {
        let with = plan_request("rest_get", &[t("path", "/rate_limit")]).unwrap();
        let without = plan_request("rest_get", &[t("path", "rate_limit")]).unwrap();
        assert_eq!(with.url, "https://api.github.com/rate_limit");
        assert_eq!(without.url, "https://api.github.com/rate_limit");
        assert_eq!(with.method, "GET");
    }

    #[test]
    fn graphql_query_posts_without_confirmation() {
        let req = plan_request("graphql", &[t("query", "query { viewer { login } }")]).unwrap();
        assert_eq!(req.method, "POST");
        assert_eq!(req.url, "https://api.github.com/graphql");
        assert_eq!(body_json(&req)["query"], "query { viewer { login } }");
        assert_eq!(header(&req, "content-type"), Some("application/json"));
    }

    #[test]
    fn graphql_includes_parsed_variables() {
        let req = plan_request(
            "graphql",
            &[
                t("query", "query($n:Int){ x }"),
                t("variables_json", r#"{"n":3}"#),
            ],
        )
        .unwrap();
        assert_eq!(body_json(&req)["variables"], json!({ "n": 3 }));
    }

    #[test]
    fn graphql_rejects_malformed_variables_json() {
        let err = plan_request(
            "graphql",
            &[t("query", "query { x }"), t("variables_json", "{not json")],
        )
        .unwrap_err();
        assert!(matches!(err, ToolError::InvalidCall(_)));
    }

    // ---------------- the confirmation gate ----------------

    #[test]
    fn is_graphql_mutation_detects_only_a_leading_mutation_keyword() {
        assert!(is_graphql_mutation("mutation { addStar }"));
        assert!(is_graphql_mutation("  \n mutation Foo { x }"));
        assert!(is_graphql_mutation("mutation"));
        assert!(!is_graphql_mutation("query { viewer { login } }"));
        // A field/type merely named like the keyword is not a mutation op.
        assert!(!is_graphql_mutation("query { mutations { total } }"));
        assert!(!is_graphql_mutation("mutationsFeed { x }"));
    }

    #[test]
    fn graphql_mutation_without_confirm_builds_no_request() {
        let err = plan_request("graphql", &[t("query", "mutation { addStar }")]).unwrap_err();
        assert!(
            matches!(err, ToolError::ConfirmationRequired(_)),
            "a graphql mutation must require confirmation, got {err:?}"
        );
    }

    #[test]
    fn graphql_mutation_with_confirm_is_planned() {
        let req = plan_request(
            "graphql",
            &[t("query", "mutation { addStar }"), b("confirm", true)],
        )
        .unwrap();
        assert_eq!(req.url, "https://api.github.com/graphql");
    }

    /// The central "an unapproved request is not sent" guarantee: every
    /// mutating tool, called WITHOUT confirm, must yield ConfirmationRequired
    /// and never a PlannedRequest.
    #[test]
    fn every_mutation_without_confirm_is_refused_before_any_request() {
        let cases: Vec<(&str, Vec<Arg>)> = vec![
            (
                "issue_create",
                vec![t("owner", "o"), t("repo", "r"), t("title", "T")],
            ),
            (
                "issue_comment",
                vec![
                    t("owner", "o"),
                    t("repo", "r"),
                    i("issue_number", 1),
                    t("body", "hi"),
                ],
            ),
            (
                "pr_create",
                vec![
                    t("owner", "o"),
                    t("repo", "r"),
                    t("title", "T"),
                    t("head", "feature"),
                    t("base", "main"),
                ],
            ),
            (
                "pr_review",
                vec![
                    t("owner", "o"),
                    t("repo", "r"),
                    i("pr_number", 2),
                    t("event", "APPROVE"),
                ],
            ),
            (
                "pr_merge",
                vec![t("owner", "o"), t("repo", "r"), i("pr_number", 3)],
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
            "issue_create",
            &[
                t("owner", "o"),
                t("repo", "r"),
                t("title", "T"),
                t("confirm", "true"),
            ],
        )
        .unwrap();
        assert_eq!(req.method, "POST");
    }

    // ---------------- mutating planning (confirmed) ----------------

    #[test]
    fn issue_create_posts_title_and_body() {
        let req = plan_request(
            "issue_create",
            &[
                t("owner", "o"),
                t("repo", "r"),
                t("title", "Bug"),
                t("body", "It broke"),
                b("confirm", true),
            ],
        )
        .unwrap();
        assert_eq!(req.method, "POST");
        assert_eq!(req.url, "https://api.github.com/repos/o/r/issues");
        let body = body_json(&req);
        assert_eq!(body["title"], "Bug");
        assert_eq!(body["body"], "It broke");
    }

    #[test]
    fn issue_comment_posts_to_the_issue_comments_endpoint() {
        let req = plan_request(
            "issue_comment",
            &[
                t("owner", "o"),
                t("repo", "r"),
                i("issue_number", 42),
                t("body", "thanks"),
                b("confirm", true),
            ],
        )
        .unwrap();
        assert_eq!(req.method, "POST");
        assert_eq!(
            req.url,
            "https://api.github.com/repos/o/r/issues/42/comments"
        );
        assert_eq!(body_json(&req)["body"], "thanks");
    }

    #[test]
    fn issue_comment_requires_an_issue_number() {
        let err = plan_request(
            "issue_comment",
            &[
                t("owner", "o"),
                t("repo", "r"),
                t("body", "x"),
                b("confirm", true),
            ],
        )
        .unwrap_err();
        assert!(matches!(err, ToolError::InvalidCall(_)));
    }

    #[test]
    fn pr_create_posts_head_and_base() {
        let req = plan_request(
            "pr_create",
            &[
                t("owner", "o"),
                t("repo", "r"),
                t("title", "Feature"),
                t("head", "feature"),
                t("base", "main"),
                t("body", "please review"),
                b("confirm", true),
            ],
        )
        .unwrap();
        assert_eq!(req.url, "https://api.github.com/repos/o/r/pulls");
        let body = body_json(&req);
        assert_eq!(body["title"], "Feature");
        assert_eq!(body["head"], "feature");
        assert_eq!(body["base"], "main");
        assert_eq!(body["body"], "please review");
    }

    #[test]
    fn pr_review_posts_event_and_body() {
        let req = plan_request(
            "pr_review",
            &[
                t("owner", "o"),
                t("repo", "r"),
                i("pr_number", 7),
                t("event", "REQUEST_CHANGES"),
                t("body", "needs work"),
                b("confirm", true),
            ],
        )
        .unwrap();
        assert_eq!(req.url, "https://api.github.com/repos/o/r/pulls/7/reviews");
        let body = body_json(&req);
        assert_eq!(body["event"], "REQUEST_CHANGES");
        assert_eq!(body["body"], "needs work");
    }

    #[test]
    fn pr_review_rejects_an_unknown_event() {
        let err = plan_request(
            "pr_review",
            &[
                t("owner", "o"),
                t("repo", "r"),
                i("pr_number", 7),
                t("event", "LGTM"),
                b("confirm", true),
            ],
        )
        .unwrap_err();
        assert!(matches!(err, ToolError::InvalidCall(_)));
    }

    #[test]
    fn pr_merge_puts_the_merge_method() {
        let req = plan_request(
            "pr_merge",
            &[
                t("owner", "o"),
                t("repo", "r"),
                i("pr_number", 9),
                t("method", "squash"),
                b("confirm", true),
            ],
        )
        .unwrap();
        assert_eq!(req.method, "PUT");
        assert_eq!(req.url, "https://api.github.com/repos/o/r/pulls/9/merge");
        assert_eq!(body_json(&req)["merge_method"], "squash");
    }

    #[test]
    fn pr_merge_defaults_to_the_merge_method() {
        let req = plan_request(
            "pr_merge",
            &[
                t("owner", "o"),
                t("repo", "r"),
                i("pr_number", 9),
                b("confirm", true),
            ],
        )
        .unwrap();
        assert_eq!(body_json(&req)["merge_method"], "merge");
    }

    #[test]
    fn pr_merge_rejects_an_unknown_method() {
        let err = plan_request(
            "pr_merge",
            &[
                t("owner", "o"),
                t("repo", "r"),
                i("pr_number", 9),
                t("method", "fast-forward"),
                b("confirm", true),
            ],
        )
        .unwrap_err();
        assert!(matches!(err, ToolError::InvalidCall(_)));
    }

    #[test]
    fn unknown_tool_is_invalid_call() {
        let err = plan_request("delete_the_repo", &[]).unwrap_err();
        assert!(matches!(err, ToolError::InvalidCall(_)));
    }

    // ---------------- response parsing ----------------

    #[test]
    fn auth_status_200_reports_connected_with_login() {
        let body = br#"{"login":"octocat","name":"The Octocat"}"#;
        let values = parse_response("auth_status", 200, body).unwrap();
        let ToolValueOut::Text(text) = &values[0] else {
            panic!("expected a text value");
        };
        let parsed: Value = serde_json::from_str(text).unwrap();
        assert_eq!(parsed["connected"], true);
        assert_eq!(parsed["login"], "octocat");
        assert_eq!(parsed["name"], "The Octocat");
    }

    #[test]
    fn auth_status_401_reports_not_connected_without_erroring() {
        let values = parse_response(
            "auth_status",
            401,
            br#"{"message":"Requires authentication"}"#,
        )
        .unwrap();
        let ToolValueOut::Text(text) = &values[0] else {
            panic!("expected a text value");
        };
        let parsed: Value = serde_json::from_str(text).unwrap();
        assert_eq!(parsed["connected"], false);
    }

    #[test]
    fn repo_get_200_summarizes_the_repository() {
        let body = br#"{
            "full_name":"octo/hello","description":"hi","stargazers_count":5,
            "forks_count":2,"open_issues_count":1,"default_branch":"main",
            "private":false,"html_url":"https://github.com/octo/hello"
        }"#;
        let values = parse_response("repo_get", 200, body).unwrap();
        let ToolValueOut::Text(text) = &values[0] else {
            panic!("expected a text value");
        };
        let parsed: Value = serde_json::from_str(text).unwrap();
        assert_eq!(parsed["full_name"], "octo/hello");
        assert_eq!(parsed["stars"], 5);
    }

    #[test]
    fn repo_get_404_is_not_found() {
        let err = parse_response("repo_get", 404, b"{}").unwrap_err();
        assert!(matches!(err, ToolError::NotFound));
    }

    #[test]
    fn issue_list_200_summarizes_each_issue() {
        let body = br#"[
            {"number":1,"title":"first","state":"open","user":{"login":"a"},
             "html_url":"https://github.com/o/r/issues/1"}
        ]"#;
        let values = parse_response("issue_list", 200, body).unwrap();
        let ToolValueOut::Text(text) = &values[0] else {
            panic!("expected a text value");
        };
        let parsed: Value = serde_json::from_str(text).unwrap();
        assert_eq!(parsed[0]["number"], 1);
        assert_eq!(parsed[0]["title"], "first");
        assert_eq!(parsed[0]["user"], "a");
    }

    #[test]
    fn pr_list_200_summarizes_head_and_base() {
        let body = br#"[
            {"number":3,"title":"pr","state":"open","user":{"login":"a"},
             "head":{"ref":"feature"},"base":{"ref":"main"},
             "html_url":"https://github.com/o/r/pull/3"}
        ]"#;
        let values = parse_response("pr_list", 200, body).unwrap();
        let ToolValueOut::Text(text) = &values[0] else {
            panic!("expected a text value");
        };
        let parsed: Value = serde_json::from_str(text).unwrap();
        assert_eq!(parsed[0]["head"], "feature");
        assert_eq!(parsed[0]["base"], "main");
    }

    #[test]
    fn issue_create_201_returns_number_and_url() {
        let body = br#"{"number":11,"html_url":"https://github.com/o/r/issues/11"}"#;
        let values = parse_response("issue_create", 201, body).unwrap();
        let ToolValueOut::Text(text) = &values[0] else {
            panic!("expected a text value");
        };
        let parsed: Value = serde_json::from_str(text).unwrap();
        assert_eq!(parsed["number"], 11);
        assert_eq!(parsed["html_url"], "https://github.com/o/r/issues/11");
    }

    #[test]
    fn pr_merge_200_reports_merged() {
        let body =
            br#"{"merged":true,"sha":"abc123","message":"Pull Request successfully merged"}"#;
        let values = parse_response("pr_merge", 200, body).unwrap();
        let ToolValueOut::Text(text) = &values[0] else {
            panic!("expected a text value");
        };
        let parsed: Value = serde_json::from_str(text).unwrap();
        assert_eq!(parsed["merged"], true);
        assert_eq!(parsed["sha"], "abc123");
    }

    #[test]
    fn rest_get_passes_the_raw_body_through() {
        let body = br#"{"rate":{"remaining":42}}"#;
        let values = parse_response("rest_get", 200, body).unwrap();
        let ToolValueOut::Text(text) = &values[0] else {
            panic!("expected a text value");
        };
        assert_eq!(text, "{\"rate\":{\"remaining\":42}}");
    }

    #[test]
    fn graphql_200_passes_the_raw_body_through() {
        let body = br#"{"data":{"viewer":{"login":"octocat"}}}"#;
        let values = parse_response("graphql", 200, body).unwrap();
        let ToolValueOut::Text(text) = &values[0] else {
            panic!("expected a text value");
        };
        assert!(text.contains("octocat"));
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
    fn a_403_rate_limit_is_a_failure() {
        let err = parse_response("issue_list", 403, br#"{"message":"rate limit"}"#).unwrap_err();
        assert!(matches!(err, ToolError::Failed(_)));
    }

    #[test]
    fn not_connected_helper_reports_disconnected() {
        let ToolValueOut::Text(text) = &not_connected()[0] else {
            panic!("expected a text value");
        };
        let parsed: Value = serde_json::from_str(text).unwrap();
        assert_eq!(parsed["connected"], false);
    }
}
