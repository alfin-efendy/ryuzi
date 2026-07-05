//! `websearch` — keyless web search via DuckDuckGo's HTML endpoint.
//!
//! No API key is required; results are scraped from the HTML SERP. Best-effort:
//! if the endpoint is unreachable or its markup changes, the tool returns an
//! error result rather than failing the turn.

use super::{truncate, PermissionSpec, Tool, ToolCtx, ToolOutput};
use async_trait::async_trait;
use serde_json::{json, Value};
use std::time::Duration;

const ENDPOINT: &str = "https://html.duckduckgo.com/html/";
const UA: &str = "Mozilla/5.0 (X11; Linux x86_64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/124.0 Safari/537.36";

pub struct WebSearch;

/// One parsed search result.
#[derive(Debug, PartialEq)]
struct Hit {
    title: String,
    url: String,
    snippet: String,
}

/// Strip HTML tags and collapse whitespace.
fn strip_tags(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut in_tag = false;
    for c in s.chars() {
        match c {
            '<' => in_tag = true,
            '>' => in_tag = false,
            _ if !in_tag => out.push(c),
            _ => {}
        }
    }
    out.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Decode a DuckDuckGo redirect href to its real target URL.
fn resolve_url(href: &str) -> String {
    let abs = if let Some(stripped) = href.strip_prefix("//") {
        format!("https://{stripped}")
    } else {
        href.to_string()
    };
    if let Ok(parsed) = url::Url::parse(&abs) {
        if let Some((_, target)) = parsed.query_pairs().find(|(k, _)| k == "uddg") {
            return target.to_string();
        }
    }
    abs
}

/// Extract results from a DuckDuckGo HTML SERP.
fn parse_results(html: &str) -> Vec<Hit> {
    // Each result anchor: <a ... class="result__a" ... href="URL">TITLE</a>
    let anchor =
        regex::Regex::new(r#"(?s)<a[^>]*class="result__a"[^>]*href="([^"]*)"[^>]*>(.*?)</a>"#)
            .expect("valid regex");
    let snippet_re = regex::Regex::new(r#"(?s)<a[^>]*class="result__snippet"[^>]*>(.*?)</a>"#)
        .expect("valid regex");
    let snippets: Vec<String> = snippet_re
        .captures_iter(html)
        .map(|c| strip_tags(&c[1]))
        .collect();
    anchor
        .captures_iter(html)
        .enumerate()
        .map(|(i, c)| Hit {
            title: strip_tags(&c[2]),
            url: resolve_url(&c[1]),
            snippet: snippets.get(i).cloned().unwrap_or_default(),
        })
        .collect()
}

#[async_trait]
impl Tool for WebSearch {
    fn name(&self) -> &str {
        "websearch"
    }
    fn description(&self) -> &str {
        "Search the web (via DuckDuckGo) and return the top results as title, \
         URL, and snippet. Use `webfetch` afterward to read a specific result."
    }
    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "query": {"type": "string", "description": "The search query."},
                "max_results": {"type": "integer", "description": "Maximum results to return (default 8)."}
            },
            "required": ["query"]
        })
    }
    fn kind(&self) -> &'static str {
        "fetch"
    }
    fn permission(&self, input: &Value) -> PermissionSpec {
        let q = input.get("query").and_then(|v| v.as_str()).unwrap_or("");
        PermissionSpec::new("webfetch", format!("web search: {q}"))
    }
    async fn execute(&self, ctx: &ToolCtx, input: Value) -> anyhow::Result<ToolOutput> {
        let query = input
            .get("query")
            .and_then(|v| v.as_str())
            .ok_or_else(|| anyhow::anyhow!("websearch: `query` is required"))?;
        let max = input
            .get("max_results")
            .and_then(|v| v.as_u64())
            .unwrap_or(8)
            .clamp(1, 20) as usize;

        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(20))
            .user_agent(UA)
            .build()
            .unwrap_or_default();
        let resp = tokio::select! {
            _ = ctx.cancel.cancelled() => return Ok(ToolOutput::error("websearch: interrupted")),
            r = client.get(ENDPOINT).query(&[("q", query)]).send() => match r {
                Ok(r) => r,
                Err(e) => return Ok(ToolOutput::error(format!("websearch: {e}"))),
            }
        };
        let html = match resp.text().await {
            Ok(h) => h,
            Err(e) => {
                return Ok(ToolOutput::error(format!(
                    "websearch: reading results: {e}"
                )))
            }
        };
        let hits = parse_results(&html);
        if hits.is_empty() {
            return Ok(ToolOutput::ok(format!("no results for `{query}`")));
        }
        let rendered = hits
            .into_iter()
            .take(max)
            .enumerate()
            .map(|(i, h)| format!("{}. {}\n   {}\n   {}", i + 1, h.title, h.url, h.snippet))
            .collect::<Vec<_>>()
            .join("\n");
        Ok(ToolOutput::ok(truncate(&rendered, &ctx.caps)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strip_tags_and_collapse() {
        assert_eq!(strip_tags("<b>Hello</b>   <i>world</i>"), "Hello world");
    }

    #[test]
    fn resolve_ddg_redirect_url() {
        let href = "//duckduckgo.com/l/?uddg=https%3A%2F%2Fexample.com%2Fpage&rut=abc";
        assert_eq!(resolve_url(href), "https://example.com/page");
        assert_eq!(
            resolve_url("https://direct.example.com"),
            "https://direct.example.com"
        );
    }

    #[test]
    fn parses_result_anchors_and_snippets() {
        let html = r#"
            <div class="result">
              <a class="result__a" href="//duckduckgo.com/l/?uddg=https%3A%2F%2Frust-lang.org">The <b>Rust</b> Language</a>
              <a class="result__snippet" href="x">A language <b>empowering</b> everyone.</a>
            </div>
            <div class="result">
              <a class="result__a" href="https://doc.rust-lang.org">Rust Docs</a>
              <a class="result__snippet" href="y">Official documentation.</a>
            </div>
        "#;
        let hits = parse_results(html);
        assert_eq!(hits.len(), 2);
        assert_eq!(hits[0].title, "The Rust Language");
        assert_eq!(hits[0].url, "https://rust-lang.org");
        assert_eq!(hits[0].snippet, "A language empowering everyone.");
        assert_eq!(hits[1].url, "https://doc.rust-lang.org");
    }
}
