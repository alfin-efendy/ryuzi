//! Cross-tool Native Tools V2 regressions driven through the public harness.
//!
//! Process timeout/background continuation belongs to the follow-up `shell` +
//! `ProcessRegistry` reliability plan. PR-diff artifact diagnosis belongs to
//! the follow-up `ArtifactLedger` scheduling/provenance plan. Neither boundary
//! is represented by a skipped test here, because that would imply coverage.
//!
//! Manual OpenAI acceptance is opt-in and uses only operator configuration:
//! `$env:RYUZI_NATIVE_TOOLS_SMOKE_MODEL='<configured Terra model id>'`, then
//! `$env:RYUZI_NATIVE_TOOLS_VERSION='v2'`, then
//! `cargo test -p ryuzi-core native_tools_v2_openai_smoke -- --ignored --nocapture`.

use async_trait::async_trait;
use ryuzi_core::agents::bootstrap::{default_ryuzi_profile, AgentPersistence};
use ryuzi_core::agents::types::{AgentModel, AgentSnapshot};
use ryuzi_core::approval::ApprovalHub;
use ryuzi_core::domain::{PermMode, Session, SessionKind, SessionStatus, WriteOrigin};
use ryuzi_core::harness::native::background::BackgroundRegistry;
use ryuzi_core::harness::native::capabilities::{
    NativeToolsVersion, TransportToolCapabilities, WireProtocol,
};
use ryuzi_core::harness::native::llm::{LlmStream, LlmStreamFactory};
use ryuzi_core::harness::native::memory::{MemoryScope, MemoryStore};
use ryuzi_core::harness::native::NativeHarness;
use ryuzi_core::harness::{Harness, SessionCtx, TurnPrompt};
use ryuzi_core::llm_router::model_effort::TurnEffortPolicy;
use ryuzi_core::llm_router::provenance::{
    AnthropicEvent, LlmRequest, RouteSelection, RouteSelectionReason, RoutedStream,
};
use ryuzi_core::telemetry::NoopTelemetry;
use ryuzi_core::Store;
use serde::Deserialize;
use serde_json::{json, Value};
use std::collections::VecDeque;
use std::path::Path;
use std::sync::{Arc, Mutex};
use tokio::sync::{broadcast, mpsc};

const SESSION_PK: &str = "native-tools-v2-regressions";
const MAIN_AGENT_ID: &str = "ryuzi";

struct ScriptedResponsesLlm {
    turns: Mutex<VecDeque<Vec<AnthropicEvent>>>,
}

impl ScriptedResponsesLlm {
    fn new(turns: Vec<Vec<AnthropicEvent>>) -> Self {
        Self {
            turns: Mutex::new(turns.into()),
        }
    }
}

#[async_trait]
impl LlmStream for ScriptedResponsesLlm {
    async fn stream(&self, request: LlmRequest) -> anyhow::Result<RoutedStream> {
        let events = self
            .turns
            .lock()
            .unwrap()
            .pop_front()
            .ok_or_else(|| anyhow::anyhow!("scripted provider has no remaining turn"))?;
        let requested_model = request.metadata.effort_policy.requested_model.clone();
        let (tx, rx) = mpsc::channel(64);
        tokio::spawn(async move {
            for event in events {
                if tx.send(Ok(event)).await.is_err() {
                    break;
                }
            }
        });
        Ok(RoutedStream {
            selection: RouteSelection {
                requested_model: requested_model.clone(),
                resolved_provider_id: "scripted".into(),
                resolved_family: "scripted".into(),
                resolved_model: requested_model.clone(),
                resolved_model_display_name: requested_model,
                effective_effort: None,
                effective_effort_label: None,
                connection_id: "scripted".into(),
                connection_label: "Scripted".into(),
                reason: RouteSelectionReason::Initial,
            },
            events: rx,
        })
    }

    async fn transport_tool_capabilities(
        &self,
        _policy: &TurnEffortPolicy,
    ) -> anyhow::Result<TransportToolCapabilities> {
        Ok(TransportToolCapabilities {
            wire_protocol: WireProtocol::OpenAiResponses,
            supports_function_tools: true,
            supports_custom_freeform_tools: false,
            supports_parallel_tool_calls: true,
            supports_strict_function_schema: true,
            supports_tool_output_schema: true,
            schema_budget_tokens: 32_000,
        })
    }
}

struct SharedFactory(Arc<dyn LlmStream>);

impl LlmStreamFactory for SharedFactory {
    fn create(&self, _store: Arc<Store>) -> Arc<dyn LlmStream> {
        self.0.clone()
    }
}

fn tool_use_start(index: i64, id: &str, name: &str) -> AnthropicEvent {
    (
        "content_block_start".into(),
        json!({
            "type": "content_block_start",
            "index": index,
            "content_block": {"type": "tool_use", "id": id, "name": name, "input": {}}
        }),
    )
}

fn input_json_delta(index: i64, input: Value) -> AnthropicEvent {
    (
        "content_block_delta".into(),
        json!({
            "type": "content_block_delta",
            "index": index,
            "delta": {"type": "input_json_delta", "partial_json": input.to_string()}
        }),
    )
}

fn message_delta(stop_reason: &str) -> AnthropicEvent {
    (
        "message_delta".into(),
        json!({
            "type": "message_delta",
            "delta": {"stop_reason": stop_reason},
            "usage": {"output_tokens": 1}
        }),
    )
}

fn message_stop() -> AnthropicEvent {
    ("message_stop".into(), json!({"type": "message_stop"}))
}

fn final_turn(text: &str) -> Vec<AnthropicEvent> {
    vec![
        (
            "content_block_delta".into(),
            json!({
                "type": "content_block_delta",
                "index": 0,
                "delta": {"type": "text_delta", "text": text}
            }),
        ),
        message_delta("end_turn"),
        message_stop(),
    ]
}

async fn fixture_context(
    store: Arc<Store>,
    work_dir: &Path,
    session_pk: &str,
    requested_model: &str,
) -> anyhow::Result<(SessionCtx, AgentPersistence)> {
    store
        .insert_session(Session {
            session_pk: session_pk.into(),
            primary_agent_id: Some(MAIN_AGENT_ID.into()),
            primary_agent_snapshot: None,
            project_id: None,
            agent_session_id: None,
            worktree_path: None,
            branch: None,
            title: Some("native tools v2 regression".into()),
            status: SessionStatus::Idle,
            perm_mode: PermMode::BypassPermissions,
            started_by: None,
            created_at: Some(0),
            last_active: Some(0),
            resume_attempts: 0,
            branch_owned: false,
            kind: SessionKind::Chat,
            speaker: None,
            agent: None,
            parent_session_pk: None,
        })
        .await?;
    let persistence = AgentPersistence::temporary(store.clone()).await?;
    let mut profile = default_ryuzi_profile(MAIN_AGENT_ID.into());
    profile.model = AgentModel::Route {
        route: requested_model.into(),
    };
    profile.permissions.mode = PermMode::BypassPermissions;
    let primary_agent = Arc::new(AgentSnapshot {
        profile,
        executable: true,
        validation: Vec::new(),
    });
    let (events, _receiver) = broadcast::channel(64);
    let delegation = ryuzi_core::delegation::DelegationRuntime::new(
        store.clone(),
        persistence.registry.clone(),
        None,
        events.clone(),
    );
    let run = delegation
        .begin_primary(
            session_pk,
            primary_agent.clone(),
            "native tools v2 regression",
        )
        .await?;

    Ok((
        SessionCtx {
            session_pk: session_pk.into(),
            primary_agent,
            run_id: run.run.run_id.clone(),
            root_run_id: run.run.run_id,
            delegation,
            main_agent_id: MAIN_AGENT_ID.into(),
            project_id: None,
            kind: SessionKind::Chat,
            agent: None,
            isolated_target: false,
            work_dir: work_dir.to_path_buf(),
            attachments_dir: None,
            perm_mode: PermMode::BypassPermissions,
            model: Some(requested_model.into()),
            effort: None,
            resume: None,
            mcp_servers: Vec::new(),
            mcp_principals: Default::default(),
            extra_skill_dirs: Vec::new(),
            extension_events: None,
            extension_tools: None,
            events,
            approvals: Arc::new(ApprovalHub::new()),
            automation_events: None,
            background: BackgroundRegistry::new(),
            agent_knowledge: persistence.knowledge.clone(),
            learning_queue: persistence.learning.clone(),
            store,
            telemetry: Arc::new(NoopTelemetry),
            app_control: None,
        },
        persistence,
    ))
}

#[derive(Debug, Deserialize)]
struct ParsedV2Envelope {
    ok: bool,
    #[serde(default)]
    data: Option<Value>,
    #[serde(default)]
    error: Option<Value>,
    meta: Value,
}

fn parse_envelope(block: &Value) -> ParsedV2Envelope {
    let envelope: ParsedV2Envelope = serde_json::from_str(
        block["content"]
            .as_str()
            .expect("V2 tool result content is JSON text"),
    )
    .expect("every tool result parses as a V2 envelope");
    assert!(envelope.meta.is_object());
    assert_eq!(envelope.ok, envelope.data.is_some());
    assert_eq!(!envelope.ok, envelope.error.is_some());
    envelope
}

#[tokio::test]
async fn five_reported_failures_return_ordered_v2_envelopes_without_side_effects() {
    let worktree = tempfile::tempdir().unwrap();
    let source_dir = worktree.path().join("apps/cockpit/src");
    std::fs::create_dir_all(&source_dir).unwrap();
    std::fs::write(
        source_dir.join("store-native.test.ts"),
        "first line\nsecond line\nthird line\n",
    )
    .unwrap();
    std::fs::write(
        source_dir.join("store-navigation.test.ts"),
        "candidate-secret-must-not-be-read\n",
    )
    .unwrap();
    std::fs::write(
        worktree.path().join("duplicate.txt"),
        "target();\nseparator\ntarget();\n",
    )
    .unwrap();
    std::fs::write(
        worktree.path().join(".git"),
        "gitdir: requested-file-secret\n",
    )
    .unwrap();
    let edit_before = std::fs::read_to_string(worktree.path().join("duplicate.txt")).unwrap();

    let first_turn = vec![
        tool_use_start(0, "call-memory-empty", "memory_batch"),
        input_json_delta(0, json!({"operations": []})),
        tool_use_start(1, "call-read-line", "read"),
        input_json_delta(
            1,
            json!({
                "path": ":2:apps/cockpit/src/store-native.test.ts",
                "offset": null,
                "limit": null
            }),
        ),
        tool_use_start(2, "call-ls-gitfile", "ls"),
        input_json_delta(2, json!({"path": ".git"})),
        tool_use_start(3, "call-edit-duplicate", "edit"),
        input_json_delta(
            3,
            json!({
                "path": "duplicate.txt",
                "old_string": "target();",
                "new_string": "replacement();",
                "replace_all": null
            }),
        ),
        tool_use_start(4, "call-read-missing", "read"),
        input_json_delta(
            4,
            json!({
                "path": "apps/cockpit/src/store-navigation.ts",
                "offset": null,
                "limit": null
            }),
        ),
        message_delta("tool_use"),
        message_stop(),
    ];
    let llm: Arc<dyn LlmStream> = Arc::new(ScriptedResponsesLlm::new(vec![
        first_turn,
        final_turn("done"),
    ]));
    let database = tempfile::NamedTempFile::new().unwrap();
    let store = Arc::new(Store::open(database.path()).await.unwrap());
    store
        .set_setting(
            WriteOrigin::User,
            "native_tools.version",
            NativeToolsVersion::V2.as_str(),
        )
        .await
        .unwrap();
    let (ctx, persistence) = fixture_context(
        store.clone(),
        worktree.path(),
        SESSION_PK,
        "configured-model",
    )
    .await
    .unwrap();
    let memory = MemoryStore::for_agent(persistence.knowledge, MAIN_AGENT_ID, None).unwrap();
    assert!(memory.load(MemoryScope::Global).await.unwrap().is_empty());
    assert!(memory.load(MemoryScope::User).await.unwrap().is_empty());

    let harness = NativeHarness::with_llm_factory(Arc::new(SharedFactory(llm)));
    let session = harness.start_session(ctx).await.unwrap();
    session
        .send_prompt(TurnPrompt::text("run regressions", "run regressions"))
        .await
        .unwrap();

    let turns = store.list_provider_turns(SESSION_PK).await.unwrap();
    let result_turn = turns
        .iter()
        .find(|turn| {
            turn.role == "user"
                && turn.payload.as_array().is_some_and(|blocks| {
                    blocks.len() == 5 && blocks.iter().all(|block| block["type"] == "tool_result")
                })
        })
        .expect("one ordered five-result provider turn");
    let blocks = result_turn.payload.as_array().unwrap();
    assert_eq!(
        blocks
            .iter()
            .map(|block| block["tool_use_id"].as_str().unwrap())
            .collect::<Vec<_>>(),
        [
            "call-memory-empty",
            "call-read-line",
            "call-ls-gitfile",
            "call-edit-duplicate",
            "call-read-missing",
        ]
    );
    let envelopes = blocks.iter().map(parse_envelope).collect::<Vec<_>>();

    assert_eq!(
        envelopes[0].error.as_ref().unwrap()["code"],
        "invalid_arguments"
    );

    assert!(envelopes[1].ok);
    let read_data = envelopes[1].data.as_ref().unwrap().as_str().unwrap();
    assert!(read_data.starts_with("     2\tsecond line"), "{read_data}");
    assert!(!read_data.contains("     1\tfirst line"));

    let ls_error = envelopes[2].error.as_ref().unwrap();
    assert_eq!(ls_error["code"], "expected_directory");
    assert_eq!(ls_error["details"]["suggested_tool"], "read");

    let edit_error = envelopes[3].error.as_ref().unwrap();
    assert_eq!(edit_error["code"], "edit_ambiguous");
    assert_eq!(
        edit_error["candidates"]
            .as_array()
            .unwrap()
            .iter()
            .map(|candidate| candidate["line"].as_u64().unwrap())
            .collect::<Vec<_>>(),
        [1, 3]
    );

    let missing_error = envelopes[4].error.as_ref().unwrap();
    assert_eq!(missing_error["code"], "path_not_found");
    assert!(missing_error["candidates"]
        .as_array()
        .unwrap()
        .iter()
        .any(|candidate| candidate["path"] == "apps/cockpit/src/store-navigation.test.ts"));

    assert_eq!(
        std::fs::read_to_string(worktree.path().join("duplicate.txt")).unwrap(),
        edit_before
    );
    assert!(!source_dir.join("store-navigation.ts").exists());
    assert!(memory.load(MemoryScope::Global).await.unwrap().is_empty());
    assert!(memory.load(MemoryScope::User).await.unwrap().is_empty());

    let rows = store.list_messages(SESSION_PK).await.unwrap();
    let normalized_read = rows
        .iter()
        .find(|row| row.tool_call_id.as_deref() == Some("call-read-line"))
        .unwrap();
    assert_eq!(
        normalized_read.payload["input"]["path"],
        "apps/cockpit/src/store-native.test.ts"
    );
    assert_eq!(normalized_read.payload["input"]["offset"], 2);
    let missing_read = rows
        .iter()
        .find(|row| row.tool_call_id.as_deref() == Some("call-read-missing"))
        .unwrap();
    assert_eq!(
        missing_read.payload["input"]["path"],
        "apps/cockpit/src/store-navigation.ts"
    );

    let persisted = serde_json::to_string(&(turns, rows)).unwrap();
    for forbidden in [
        "os error",
        "requested-file-secret",
        "candidate-secret-must-not-be-read",
        worktree.path().to_string_lossy().as_ref(),
    ] {
        assert!(
            !persisted.contains(forbidden),
            "persisted raw detail: {forbidden}"
        );
    }
}

#[tokio::test]
#[ignore = "manual OpenAI API-key acceptance; requires explicit environment configuration"]
async fn native_tools_v2_openai_smoke() {
    let Ok(model) = std::env::var("RYUZI_NATIVE_TOOLS_SMOKE_MODEL") else {
        eprintln!("SKIP: RYUZI_NATIVE_TOOLS_SMOKE_MODEL is not configured");
        return;
    };
    let Ok(version) = std::env::var("RYUZI_NATIVE_TOOLS_VERSION") else {
        eprintln!("SKIP: RYUZI_NATIVE_TOOLS_VERSION is not configured");
        return;
    };
    assert_eq!(
        NativeToolsVersion::parse(version.trim()).unwrap(),
        NativeToolsVersion::V2,
        "RYUZI_NATIVE_TOOLS_VERSION must be exactly v2"
    );
    if model.trim().is_empty() {
        eprintln!("SKIP: RYUZI_NATIVE_TOOLS_SMOKE_MODEL is blank");
        return;
    }

    let configured_path = ryuzi_core::paths::db_path();
    if !configured_path.exists() {
        eprintln!("SKIP: configured Ryuzi settings store is absent");
        return;
    }
    let configured_store = Store::open(&configured_path).await.unwrap();
    let Some(mut connection) =
        ryuzi_core::llm_router::connections::list_connections(&configured_store)
            .await
            .unwrap()
            .into_iter()
            .find(|connection| {
                connection.enabled
                    && connection.provider == "openai"
                    && connection.auth_type == "api_key"
                    && connection
                        .data
                        .api_key
                        .as_deref()
                        .is_some_and(|key| !key.is_empty())
            })
    else {
        eprintln!("SKIP: no enabled OpenAI API-key connection is configured");
        return;
    };

    let database = tempfile::NamedTempFile::new().unwrap();
    let store = Arc::new(Store::open(database.path()).await.unwrap());
    connection.id = "native-tools-v2-openai-smoke".into();
    connection.label = "Native Tools V2 smoke".into();
    ryuzi_core::llm_router::connections::add_connection(&store, connection)
        .await
        .unwrap();
    ryuzi_core::llm_router::routes::save_model_route(
        &store,
        ryuzi_core::llm_router::routes::ModelRouteInfo {
            id: String::new(),
            name: "native-tools-v2-openai-smoke".into(),
            enabled: true,
            strategy: ryuzi_core::llm_router::routes::ModelRouteStrategy::Fallback,
            targets: vec![ryuzi_core::llm_router::routes::ModelRouteTarget {
                provider: "openai".into(),
                model: model.trim().into(),
                effort: None,
            }],
            created_at: 0,
            updated_at: 0,
        },
    )
    .await
    .unwrap();
    store
        .set_setting(
            WriteOrigin::User,
            "native_tools.version",
            NativeToolsVersion::V2.as_str(),
        )
        .await
        .unwrap();

    let worktree = tempfile::tempdir().unwrap();
    let (ctx, _persistence) = fixture_context(
        store.clone(),
        worktree.path(),
        "native-tools-v2-openai-smoke",
        "native-tools-v2-openai-smoke",
    )
    .await
    .unwrap();
    let session = NativeHarness::new().start_session(ctx).await.unwrap();
    session
        .send_prompt(TurnPrompt::text(
            "Reply with exactly SMOKE_OK and do not call a tool.",
            "Reply with exactly SMOKE_OK and do not call a tool.",
        ))
        .await
        .unwrap();
    eprintln!("PASS: configured OpenAI model accepted the Native Tools V2 request");
}
