//! Single-model probe: the real chat request capped at 1 output token.
//!
//! The Cockpit "test model" flow used to hand-roll a second copy of the
//! upstream request builders and drifted from the real chat path (no kiro
//! branch at all, `chat_path` ignored, `max_tokens` sent to OpenAI). Probes
//! now go through the SAME builders chat uses ([`client::upstream_request`],
//! and the kiro/codex pipelines added alongside), so a probe succeeds or
//! fails exactly like a real one-token completion would. Only the HTTP
//! status is read; the response body is dropped unread.

use std::sync::Arc;

use serde_json::{json, Value};

use crate::llm_router::client::{self, RouteTarget, UpstreamCtx};
use crate::llm_router::connections::{self, ConnectionRow};
use crate::llm_router::models::{probe_status, ProbeStatus};
use crate::llm_router::oauth;
use crate::llm_router::registry::ProviderDescriptor;
use crate::store::Store;

/// Outcome of a single-model probe. `status` follows the tri-state mapping of
/// [`crate::llm_router::models::probe_status`]; `ok` is always
/// `status == ProbeStatus::Valid`.
#[derive(Debug, Clone)]
pub struct ProbeOutcome {
    pub ok: bool,
    pub status: ProbeStatus,
    pub message: String,
}

/// Map the probe's HTTP status (or transport error text) to the user-facing
/// verdict + message. Wording matches the Cockpit "test model" flow verbatim
/// — the Tauri layer forwards these strings unchanged.
fn probe_outcome_for(model: &str, resp: Result<reqwest::StatusCode, String>) -> ProbeOutcome {
    let status = probe_status(resp.as_ref().ok().map(|s| s.as_u16()));
    let message = match &resp {
        Ok(s) if s.is_success() => format!("Model {model} OK"),
        Ok(s) if s.as_u16() == 401 || s.as_u16() == 403 => {
            format!("Model {model} was rejected by provider credentials.")
        }
        Ok(s) => format!("Model {model} returned HTTP {s}"),
        Err(e) => format!("Model {model} network error: {e}"),
    };
    ProbeOutcome {
        ok: status == ProbeStatus::Valid,
        status,
        message,
    }
}

/// The one-token ping body in the provider's wire format. Both wire formats
/// share these field names; the OpenAI `max_tokens` → `max_completion_tokens`
/// rename is applied per-descriptor — the same rename real chat applies.
fn probe_body(desc: &ProviderDescriptor, model: &str) -> Value {
    let mut body = json!({
        "model": model,
        "messages": [{"role": "user", "content": "ping"}],
        "max_tokens": 1,
        "stream": false
    });
    client::apply_max_completion_tokens(desc, &mut body);
    body
}

/// Build the probe request for `target` — the real chat request builders
/// with a ping body.
fn probe_request(
    ctx: &UpstreamCtx,
    target: &RouteTarget,
    model: &str,
) -> anyhow::Result<reqwest::RequestBuilder> {
    match target.conn.provider.as_str() {
        // Kiro has no base URL by design — the probe goes through the same
        // CodeWhisperer translator + endpoint list as real chat; the
        // Anthropic body's `max_tokens: 1` becomes `inferenceConfig.
        // maxTokens: 1`, and the profile ARN is attached by the translator.
        "kiro" => {
            let anthropic_body = json!({
                "model": model,
                "messages": [{"role": "user", "content": "ping"}],
                "max_tokens": 1,
                "stream": false
            });
            let conversation_id = uuid::Uuid::new_v4().to_string();
            let kiro_body = crate::llm_router::kiro::anthropic_request_to_kiro(
                &anthropic_body,
                model,
                &target.conn.data,
                &conversation_id,
            );
            Ok(client::kiro_upstream_request(ctx, target, &kiro_body))
        }
        // Codex speaks the Responses wire; picker ids may carry effort or
        // `-review` suffixes the upstream doesn't know — probe the base
        // model (suffixed variants inherit its verdict).
        "openai-oauth" => {
            let body = json!({
                "model": crate::llm_router::codex::codex_base_model(model),
                "input": "ping",
                "stream": false,
                "store": false
            });
            client::upstream_request(ctx, target, &body)
        }
        // Everything else — api-key, free/no-auth (incl. mimo's `/chat`
        // path), anthropic-oauth (system-prompt injection + cloak live in
        // `upstream_request`), qwen, github-copilot — is the generic
        // real-chat builder with a ping body.
        _ => client::upstream_request(ctx, target, &probe_body(target.desc, model)),
    }
}

async fn probe_once(
    ctx: &UpstreamCtx,
    target: &RouteTarget,
    model: &str,
) -> anyhow::Result<reqwest::StatusCode> {
    Ok(probe_request(ctx, target, model)?.send().await?.status())
}

/// Probe `model` on `conn`: send the real one-token chat request, read only
/// the HTTP status, and map it to a tri-state verdict. Refresh behavior
/// mirrors the Cockpit test path exactly: proactive `ensure_fresh` first
/// (errors ignored unless the connection is terminally `needs_relogin`),
/// then ONE reactive `force_refresh` + resend on 401/403 when a refresh
/// token exists (a failed reactive refresh surfaces as a network error).
pub async fn probe_model(
    http: &reqwest::Client,
    store: &Arc<Store>,
    desc: &'static ProviderDescriptor,
    conn: &ConnectionRow,
    model: &str,
) -> ProbeOutcome {
    let ctx = UpstreamCtx {
        store: store.clone(),
        http: http.clone(),
        oauth_token_url_override: None,
    };
    let mut target = RouteTarget {
        conn: conn.clone(),
        desc,
        upstream_model: model.to_string(),
    };
    if connections::is_oauth(&target.conn) {
        if let Err(err) = oauth::refresh::ensure_fresh(store, http, &mut target.conn).await {
            if target.conn.data.needs_relogin == Some(true) {
                return probe_outcome_for(model, Err(err.to_string()));
            }
        }
    }
    let status = match probe_once(&ctx, &target, model).await {
        Ok(s) => s,
        Err(e) => return probe_outcome_for(model, Err(e.to_string())),
    };
    if connections::is_oauth(&target.conn)
        && matches!(status.as_u16(), 401 | 403)
        && target.conn.data.refresh_token.is_some()
    {
        if let Err(e) = oauth::refresh::force_refresh(store, http, &mut target.conn).await {
            return probe_outcome_for(model, Err(e.to_string()));
        }
        return match probe_once(&ctx, &target, model).await {
            Ok(s) => probe_outcome_for(model, Ok(s)),
            Err(e) => probe_outcome_for(model, Err(e.to_string())),
        };
    }
    probe_outcome_for(model, Ok(status))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::llm_router::client::{RouteTarget, UpstreamCtx};
    use crate::llm_router::connections::{ConnectionData, ConnectionRow};
    use crate::llm_router::registry;
    use serde_json::json;

    fn mk_conn(id: &str, provider: &str, auth_type: &str, data: ConnectionData) -> ConnectionRow {
        ConnectionRow {
            id: id.into(),
            provider: provider.into(),
            auth_type: auth_type.into(),
            label: "t".into(),
            priority: 0,
            enabled: true,
            data,
            created_at: 0,
            updated_at: 0,
        }
    }

    async fn test_ctx() -> UpstreamCtx {
        let tmp = tempfile::NamedTempFile::new().unwrap();
        let store = Arc::new(crate::store::Store::open(tmp.path()).await.unwrap());
        UpstreamCtx::new(store)
    }

    fn status(code: u16) -> reqwest::StatusCode {
        reqwest::StatusCode::from_u16(code).unwrap()
    }

    #[test]
    fn probe_body_renames_max_tokens_only_for_openai() {
        let openai = probe_body(registry::descriptor("openai").unwrap(), "gpt-test");
        assert_eq!(openai["max_completion_tokens"], 1);
        assert!(openai.get("max_tokens").is_none());
        assert_eq!(openai["messages"][0]["content"], "ping");
        assert_eq!(openai["stream"], false);

        let mimo = probe_body(registry::descriptor("mimo-free").unwrap(), "mimo-auto");
        assert_eq!(mimo["max_tokens"], 1);
        assert!(mimo.get("max_completion_tokens").is_none());

        let anthropic = probe_body(registry::descriptor("anthropic").unwrap(), "claude-test");
        assert_eq!(anthropic["max_tokens"], 1);
        assert!(anthropic.get("max_completion_tokens").is_none());
    }

    #[test]
    fn probe_outcome_messages_match_the_cockpit_wording() {
        let ok = probe_outcome_for("gpt-test", Ok(status(200)));
        assert!(ok.ok);
        assert_eq!(ok.status, ProbeStatus::Valid);
        assert_eq!(ok.message, "Model gpt-test OK");

        let denied = probe_outcome_for("gpt-test", Ok(status(401)));
        assert!(!denied.ok);
        assert_eq!(denied.status, ProbeStatus::Invalid);
        assert_eq!(
            denied.message,
            "Model gpt-test was rejected by provider credentials."
        );

        let missing = probe_outcome_for("gpt-test", Ok(status(404)));
        assert_eq!(missing.status, ProbeStatus::Invalid);
        assert_eq!(
            missing.message,
            "Model gpt-test returned HTTP 404 Not Found"
        );

        let flaky = probe_outcome_for("gpt-test", Ok(status(500)));
        assert_eq!(flaky.status, ProbeStatus::Unknown);

        let dead = probe_outcome_for("gpt-test", Err("connection refused".into()));
        assert_eq!(dead.status, ProbeStatus::Unknown);
        assert_eq!(
            dead.message,
            "Model gpt-test network error: connection refused"
        );
    }

    #[tokio::test]
    async fn openai_probe_request_uses_max_completion_tokens_and_bearer() {
        let ctx = test_ctx().await;
        let desc = registry::descriptor("openai").unwrap();
        let target = RouteTarget {
            conn: mk_conn(
                "c1",
                "openai",
                "api_key",
                ConnectionData {
                    api_key: Some("sk-live".into()),
                    ..Default::default()
                },
            ),
            desc,
            upstream_model: "gpt-5.2".into(),
        };
        let req = probe_request(&ctx, &target, "gpt-5.2")
            .unwrap()
            .build()
            .unwrap();
        assert_eq!(
            req.url().as_str(),
            "https://api.openai.com/v1/chat/completions"
        );
        assert_eq!(
            req.headers().get("authorization").unwrap(),
            "Bearer sk-live"
        );
        let sent: Value = serde_json::from_slice(req.body().unwrap().as_bytes().unwrap()).unwrap();
        assert_eq!(sent["model"], "gpt-5.2");
        assert_eq!(sent["max_completion_tokens"], 1);
        assert!(sent.get("max_tokens").is_none());
    }

    #[tokio::test]
    async fn mimo_probe_request_honors_nonstandard_chat_path() {
        let ctx = test_ctx().await;
        let desc = registry::descriptor("mimo-free").unwrap();
        let target = RouteTarget {
            conn: mk_conn("c2", "mimo-free", "free", ConnectionData::default()),
            desc,
            upstream_model: "mimo-auto".into(),
        };
        let req = probe_request(&ctx, &target, "mimo-auto")
            .unwrap()
            .build()
            .unwrap();
        assert_eq!(
            req.url().as_str(),
            "https://api.xiaomimimo.com/api/free-ai/openai/chat"
        );
        assert!(req.headers().get("authorization").is_none());
        let sent: Value = serde_json::from_slice(req.body().unwrap().as_bytes().unwrap()).unwrap();
        assert_eq!(sent["max_tokens"], 1);
    }

    #[tokio::test]
    async fn anthropic_probe_request_keeps_max_tokens_and_x_api_key() {
        let ctx = test_ctx().await;
        let desc = registry::descriptor("anthropic").unwrap();
        let target = RouteTarget {
            conn: mk_conn(
                "c3",
                "anthropic",
                "api_key",
                ConnectionData {
                    api_key: Some("sk-ant".into()),
                    ..Default::default()
                },
            ),
            desc,
            upstream_model: "claude-sonnet-4-5".into(),
        };
        let req = probe_request(&ctx, &target, "claude-sonnet-4-5")
            .unwrap()
            .build()
            .unwrap();
        assert_eq!(req.url().as_str(), "https://api.anthropic.com/v1/messages");
        assert_eq!(req.headers().get("x-api-key").unwrap(), "sk-ant");
        assert_eq!(
            req.headers().get("anthropic-version").unwrap(),
            "2023-06-01"
        );
        let sent: Value = serde_json::from_slice(req.body().unwrap().as_bytes().unwrap()).unwrap();
        assert_eq!(sent["max_tokens"], 1);
    }

    #[tokio::test]
    async fn probe_model_maps_upstream_status_to_verdict_over_the_wire() {
        use axum::{routing::post, Json, Router};

        async fn handler(Json(body): Json<Value>) -> (axum::http::StatusCode, Json<Value>) {
            // The probe of a real `openai` connection must carry the rename.
            assert_eq!(body["max_completion_tokens"], 1);
            assert!(body.get("max_tokens").is_none());
            if body["model"] == "gpt-good" {
                (
                    axum::http::StatusCode::OK,
                    Json(json!({"id": "chatcmpl-1", "choices": []})),
                )
            } else {
                (
                    axum::http::StatusCode::NOT_FOUND,
                    Json(json!({"error": {"message": "model not found"}})),
                )
            }
        }

        let app = Router::new().route("/v1/chat/completions", post(handler));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });

        let tmp = tempfile::NamedTempFile::new().unwrap();
        let store = Arc::new(crate::store::Store::open(tmp.path()).await.unwrap());
        let http = reqwest::Client::new();
        let desc = registry::descriptor("openai").unwrap();
        let conn = mk_conn(
            "c1",
            "openai",
            "api_key",
            ConnectionData {
                api_key: Some("sk-live".into()),
                base_url_override: Some(format!("http://127.0.0.1:{port}/v1")),
                ..Default::default()
            },
        );

        let good = probe_model(&http, &store, desc, &conn, "gpt-good").await;
        assert!(good.ok);
        assert_eq!(good.status, ProbeStatus::Valid);
        assert_eq!(good.message, "Model gpt-good OK");

        let bad = probe_model(&http, &store, desc, &conn, "gpt-bad").await;
        assert!(!bad.ok);
        assert_eq!(bad.status, ProbeStatus::Invalid);
        assert_eq!(bad.message, "Model gpt-bad returned HTTP 404 Not Found");
    }

    #[tokio::test]
    async fn kiro_probe_needs_no_base_url_and_caps_max_tokens_at_one() {
        let ctx = test_ctx().await;
        let desc = registry::descriptor("kiro").unwrap();
        assert!(desc.base_url.is_none(), "kiro has no base URL by design");
        let target = RouteTarget {
            conn: mk_conn(
                "k1",
                "kiro",
                "oauth",
                ConnectionData {
                    access_token: Some("at-kiro".into()),
                    ..Default::default()
                },
            ),
            desc,
            upstream_model: "claude-sonnet-5".into(),
        };
        let req = probe_request(&ctx, &target, "claude-sonnet-5")
            .unwrap()
            .build()
            .unwrap();
        assert_eq!(
            req.url().as_str(),
            "https://runtime.us-east-1.kiro.dev/generateAssistantResponse"
        );
        assert_eq!(
            req.headers().get("authorization").unwrap(),
            "Bearer at-kiro"
        );
        assert_eq!(
            req.headers().get("x-amz-target").unwrap(),
            "AmazonCodeWhispererStreamingService.GenerateAssistantResponse"
        );
        let sent: Value = serde_json::from_slice(req.body().unwrap().as_bytes().unwrap()).unwrap();
        assert_eq!(sent["inferenceConfig"]["maxTokens"], 1);
        let cur = &sent["conversationState"]["currentMessage"]["userInputMessage"];
        assert_eq!(cur["modelId"], "claude-sonnet-5");
        assert!(cur["content"].as_str().unwrap().contains("ping"));
        assert_eq!(
            sent["profileArn"],
            "arn:aws:codewhisperer:us-east-1:638616132270:profile/AAAACCCCXXXX"
        );
    }

    #[tokio::test]
    async fn codex_probe_hits_responses_and_normalizes_suffixed_ids() {
        let ctx = test_ctx().await;
        let desc = registry::descriptor("openai-oauth").unwrap();
        let target = RouteTarget {
            conn: mk_conn(
                "cx",
                "openai-oauth",
                "oauth",
                ConnectionData {
                    access_token: Some("at-codex".into()),
                    provider_specific: Some(json!({"chatgpt_account_id": "acct-1"})),
                    ..Default::default()
                },
            ),
            desc,
            upstream_model: "gpt-5.2-codex-high".into(),
        };
        let req = probe_request(&ctx, &target, "gpt-5.2-codex-high")
            .unwrap()
            .build()
            .unwrap();
        assert_eq!(
            req.url().as_str(),
            "https://chatgpt.com/backend-api/codex/responses"
        );
        assert_eq!(
            req.headers().get("authorization").unwrap(),
            "Bearer at-codex"
        );
        assert_eq!(req.headers().get("originator").unwrap(), "codex_cli_rs");
        assert_eq!(req.headers().get("chatgpt-account-id").unwrap(), "acct-1");
        let sent: Value = serde_json::from_slice(req.body().unwrap().as_bytes().unwrap()).unwrap();
        assert_eq!(sent["model"], "gpt-5.2-codex");
        assert_eq!(sent["input"], "ping");
        assert_eq!(sent["store"], false);
        assert_eq!(sent["stream"], false);
    }

    #[tokio::test]
    async fn codex_probe_strips_review_suffix_too() {
        let ctx = test_ctx().await;
        let desc = registry::descriptor("openai-oauth").unwrap();
        let target = RouteTarget {
            conn: mk_conn(
                "cx2",
                "openai-oauth",
                "oauth",
                ConnectionData {
                    access_token: Some("at-codex".into()),
                    ..Default::default()
                },
            ),
            desc,
            upstream_model: "gpt-5.4-review".into(),
        };
        let req = probe_request(&ctx, &target, "gpt-5.4-review")
            .unwrap()
            .build()
            .unwrap();
        let sent: Value = serde_json::from_slice(req.body().unwrap().as_bytes().unwrap()).unwrap();
        assert_eq!(sent["model"], "gpt-5.4");
    }

    #[tokio::test]
    async fn anthropic_oauth_probe_injects_system_prompt_and_beta_headers() {
        let ctx = test_ctx().await;
        let desc = registry::descriptor("anthropic-oauth").unwrap();
        let target = RouteTarget {
            conn: mk_conn(
                "a1",
                "anthropic-oauth",
                "oauth",
                ConnectionData {
                    access_token: Some("at-claude".into()),
                    ..Default::default()
                },
            ),
            desc,
            upstream_model: "claude-opus-4-8".into(),
        };
        let req = probe_request(&ctx, &target, "claude-opus-4-8")
            .unwrap()
            .build()
            .unwrap();
        assert_eq!(
            req.url().as_str(),
            "https://api.anthropic.com/v1/messages?beta=true"
        );
        assert_eq!(
            req.headers().get("authorization").unwrap(),
            "Bearer at-claude"
        );
        assert_eq!(
            req.headers().get("anthropic-beta").unwrap(),
            crate::llm_router::models::ANTHROPIC_OAUTH_BETA
        );
        let sent: Value = serde_json::from_slice(req.body().unwrap().as_bytes().unwrap()).unwrap();
        assert_eq!(
            sent["system"][0]["text"],
            crate::llm_router::models::CLAUDE_CODE_SYSTEM_PROMPT
        );
        assert_eq!(sent["max_tokens"], 1);
    }

    #[tokio::test]
    async fn anthropic_oauth_probe_applies_cloak_when_enabled() {
        let ctx = test_ctx().await;
        let desc = registry::descriptor("anthropic-oauth").unwrap();
        let target = RouteTarget {
            conn: mk_conn(
                "a2",
                "anthropic-oauth",
                "oauth",
                ConnectionData {
                    access_token: Some("sk-ant-oat-test".into()),
                    provider_specific: Some(json!({"claudeCloaking": true})),
                    ..Default::default()
                },
            ),
            desc,
            upstream_model: "claude-opus-4-8".into(),
        };
        let req = probe_request(&ctx, &target, "claude-opus-4-8")
            .unwrap()
            .build()
            .unwrap();
        assert!(req.headers().contains_key("x-claude-code-session-id"));
        assert!(req.headers().contains_key("x-stainless-runtime-version"));
    }
}
