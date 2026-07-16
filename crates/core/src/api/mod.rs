//! The engine's callable command surface: every operation Cockpit's Tauri
//! layer proxies lands here as `dispatch(state, method, params)`. Method
//! names match the Tauri command names 1:1; params objects use the Rust
//! snake_case parameter names. One submodule per command family.

pub mod agent_api;
pub mod apps_api;
pub mod audit;
pub mod automation_api;
pub mod connections_api;
pub mod delegation_api;
pub mod endpoint_api;
pub mod extension_status_api;
pub mod fsview_api;
pub mod gateways_api;
pub mod learning_api;
pub mod native_api;
pub mod plugins_api;
pub mod remote_catalog_api;
pub mod scheduler_api;
pub mod session_io_api;
pub mod sessions;
pub mod skills_api;
pub mod types;

use crate::serve::ApiState;
use serde::de::DeserializeOwned;
use serde_json::Value;

#[derive(Debug)]
pub struct ApiError {
    pub status: u16,
    pub message: String,
}

impl ApiError {
    pub fn bad_request(message: impl Into<String>) -> Self {
        ApiError {
            status: 400,
            message: message.into(),
        }
    }

    pub fn not_found(message: impl Into<String>) -> Self {
        ApiError {
            status: 404,
            message: message.into(),
        }
    }

    pub fn conflict(message: impl Into<String>) -> Self {
        ApiError {
            status: 409,
            message: message.into(),
        }
    }
}

impl From<crate::agents::registry::AgentRegistryError> for ApiError {
    fn from(e: crate::agents::registry::AgentRegistryError) -> Self {
        use crate::agents::registry::AgentRegistryError;
        match e {
            AgentRegistryError::LastAgent => {
                ApiError::conflict("at least one main agent must remain")
            }
            AgentRegistryError::NotFound(_) => ApiError::not_found(e.to_string()),
            AgentRegistryError::DuplicateId(_) | AgentRegistryError::DuplicateName(_) => {
                ApiError::bad_request(e.to_string())
            }
            AgentRegistryError::Invalid(issues) => ApiError::bad_request(
                issues
                    .iter()
                    .map(|issue| format!("{}: {}", issue.field, issue.message))
                    .collect::<Vec<_>>()
                    .join("; "),
            ),
            AgentRegistryError::Io(error) => ApiError {
                status: 500,
                message: error.to_string(),
            },
        }
    }
}

impl From<crate::mentions::MentionError> for ApiError {
    fn from(e: crate::mentions::MentionError) -> Self {
        ApiError::bad_request(e.to_string())
    }
}

impl From<crate::sessions::ownership::SessionAccessError> for ApiError {
    fn from(e: crate::sessions::ownership::SessionAccessError) -> Self {
        use crate::sessions::ownership::SessionAccessError;
        match e {
            // Read-only historical session (legacy or deleted owner): a 409
            // conflict carrying the exact user-facing message.
            SessionAccessError::ReadOnly(message) => ApiError::conflict(message),
            // Unknown / corrupt session ownership: surfaces as a 500 the same
            // way the underlying resolve error did before this guard existed.
            SessionAccessError::Resolve(message) => ApiError {
                status: 500,
                message,
            },
        }
    }
}

impl From<anyhow::Error> for ApiError {
    fn from(e: anyhow::Error) -> Self {
        // A read-only rejection can bubble up through a ControlPlane
        // `anyhow::Result`; recover its exact 409 rather than a blanket 500.
        if let Some(access) = e.downcast_ref::<crate::sessions::ownership::SessionAccessError>() {
            return ApiError::from(access.clone());
        }
        if let Some(mention) = e.downcast_ref::<crate::mentions::MentionError>() {
            return ApiError::from(mention.clone());
        }
        ApiError {
            status: 500,
            message: e.to_string(),
        }
    }
}

/// Decode an RPC call's params object into a typed request. Malformed params
/// become a 400 [`ApiError`] rather than a panic or a 500. Every command
/// family decodes its params through this.
pub(crate) fn params<T: DeserializeOwned>(v: Value) -> Result<T, ApiError> {
    serde_json::from_value(v).map_err(|e| ApiError::bad_request(format!("bad params: {e}")))
}

/// Serialize a command's `Ok` value as-is into the RPC result envelope.
pub(crate) fn ok<T: serde::Serialize>(v: T) -> Result<Value, ApiError> {
    serde_json::to_value(v).map_err(|e| ApiError {
        status: 500,
        message: e.to_string(),
    })
}

/// Route one `POST /rpc/{method}` call to its command family. Method names
/// match Cockpit's Tauri command names 1:1; later tasks chain further family
/// submodules here in the same style.
pub async fn dispatch(state: &ApiState, method: &str, p: Value) -> Result<Value, ApiError> {
    match method {
        m if sessions::HANDLES.contains(&m) => sessions::dispatch(state, m, p).await,
        m if scheduler_api::HANDLES.contains(&m) => scheduler_api::dispatch(state, m, p).await,
        m if automation_api::HANDLES.contains(&m) => automation_api::dispatch(state, m, p).await,
        m if gateways_api::HANDLES.contains(&m) => gateways_api::dispatch(state, m, p).await,
        m if apps_api::HANDLES.contains(&m) => apps_api::dispatch(state, m, p).await,
        m if native_api::HANDLES.contains(&m) => native_api::dispatch(state, m, p).await,
        m if agent_api::HANDLES.contains(&m) => agent_api::dispatch(state, m, p).await,
        m if delegation_api::HANDLES.contains(&m) => delegation_api::dispatch(state, m, p).await,
        m if session_io_api::HANDLES.contains(&m) => session_io_api::dispatch(state, m, p).await,
        m if fsview_api::HANDLES.contains(&m) => fsview_api::dispatch(state, m, p).await,
        m if skills_api::HANDLES.contains(&m) => skills_api::dispatch(state, m, p).await,
        m if endpoint_api::HANDLES.contains(&m) => endpoint_api::dispatch(state, m, p).await,
        m if connections_api::HANDLES.contains(&m) => connections_api::dispatch(state, m, p).await,
        m if plugins_api::HANDLES.contains(&m) => plugins_api::dispatch(state, m, p).await,
        m if learning_api::HANDLES.contains(&m) => learning_api::dispatch(state, m, p).await,
        m if audit::HANDLES.contains(&m) => audit::dispatch(state, m, p).await,
        m if remote_catalog_api::HANDLES.contains(&m) => {
            remote_catalog_api::dispatch(state, m, p).await
        }
        m if extension_status_api::HANDLES.contains(&m) => {
            extension_status_api::dispatch(state, m, p).await
        }
        _ => Err(ApiError::not_found(format!("unknown method: {method}"))),
    }
}

/// Shared test scaffolding for this module and every later command-family
/// module under `api/`. `pub(crate)` so sibling `api::*` test modules can
/// build an `ApiState` without duplicating the boilerplate.
#[cfg(test)]
pub(crate) mod tests_support {
    use crate::harness::{Harness, HarnessFactory, HarnessSession, SessionCtx, TurnPrompt};
    use crate::serve::ApiState;
    use async_trait::async_trait;
    use std::sync::Arc;

    async fn prepare_test_agent_persistence(store: &Arc<crate::store::Store>) {
        crate::llm_router::connections::add_connection(
            store,
            crate::llm_router::connections::ConnectionRow {
                id: "test-anthropic".into(),
                provider: "anthropic".into(),
                auth_type: "api_key".into(),
                label: "Test Anthropic".into(),
                priority: 0,
                enabled: true,
                data: crate::llm_router::connections::ConnectionData {
                    api_key: Some("test-key".into()),
                    models_override: Some(vec!["claude-opus-4-8".into()]),
                    ..Default::default()
                },
                created_at: 0,
                updated_at: 0,
            },
        )
        .await
        .unwrap();
        crate::agents::bootstrap::ensure_default_routes(store)
            .await
            .unwrap();
    }

    /// A fresh in-memory-backed `ApiState` with a real bearer token ("t").
    /// Leaks the backing tempfile's guard for test simplicity — acceptable
    /// since each test process exits shortly after.
    pub(crate) async fn state() -> ApiState {
        let tmp = tempfile::NamedTempFile::new().unwrap();
        let store = Arc::new(crate::store::Store::open(tmp.path()).await.unwrap());
        prepare_test_agent_persistence(&store).await;
        let persistence = crate::agents::bootstrap::AgentPersistence::temporary(Arc::clone(&store))
            .await
            .unwrap();
        let cp = crate::control::ControlPlane::new(
            store,
            crate::plugins::Registries::new(),
            persistence.clone(),
        )
        .await;
        std::mem::forget(tmp);
        ApiState {
            router_server: Arc::new(crate::llm_router::server::RouterServer::new(
                cp.store().clone(),
            )),
            cp,
            agents: persistence.registry,
            agent_knowledge: persistence.knowledge,
            learning_queue: persistence.learning,
            control_token: "t".into(),
        }
    }

    /// Like `state()`, but with the `smart` and `fast` model routes seeded
    /// before agent persistence bootstraps, so the default Ryuzi profile
    /// (route `smart`) and the subagent config (route `fast`) validate as
    /// executable — required by registry mutations, which re-validate the
    /// whole candidate registry.
    pub(crate) async fn state_with_agents() -> ApiState {
        state_with_agents_and_registries(crate::plugins::Registries::new()).await
    }

    pub(crate) async fn state_with_native_llm(
        llm_factory: Arc<dyn crate::harness::native::llm::LlmStreamFactory>,
    ) -> ApiState {
        let mut registries = crate::plugins::Registries::new();
        registries.harness =
            Arc::new(crate::harness::native::NativeHarnessFactory::with_llm_factory(llm_factory));
        state_with_agents_and_registries(registries).await
    }

    async fn state_with_agents_and_registries(registries: crate::plugins::Registries) -> ApiState {
        use crate::llm_router::routes::{
            self, ModelRouteInfo, ModelRouteStrategy, ModelRouteTarget,
        };
        let tmp = tempfile::NamedTempFile::new().unwrap();
        let store = Arc::new(crate::store::Store::open(tmp.path()).await.unwrap());
        for name in ["smart", "fast"] {
            routes::save_model_route(
                &store,
                ModelRouteInfo {
                    id: String::new(),
                    name: name.into(),
                    enabled: true,
                    strategy: ModelRouteStrategy::Fallback,
                    targets: vec![ModelRouteTarget {
                        provider: "anthropic".into(),
                        model: "claude-opus-4-8".into(),
                        effort: None,
                    }],
                    created_at: 0,
                    updated_at: 0,
                },
            )
            .await
            .unwrap();
        }
        let persistence = crate::agents::bootstrap::AgentPersistence::temporary(Arc::clone(&store))
            .await
            .unwrap();
        let cp = crate::control::ControlPlane::new(store, registries, persistence.clone()).await;
        std::mem::forget(tmp);
        ApiState {
            router_server: Arc::new(crate::llm_router::server::RouterServer::new(
                cp.store().clone(),
            )),
            cp,
            agents: persistence.registry,
            agent_knowledge: persistence.knowledge,
            learning_queue: persistence.learning,
            control_token: "t".into(),
        }
    }

    /// Like `state_with_agents()`, but with one connected project (`"p1"`)
    /// already in the store — for command families whose calls need a real
    /// `project_id` to validate against.
    pub(crate) async fn state_with_project() -> ApiState {
        let state = state_with_agents().await;
        state
            .cp
            .store()
            .insert_project(crate::domain::Project {
                project_id: "p1".into(),
                name: "project".into(),
                workdir: std::env::temp_dir().display().to_string(),
                source: None,
                model: None,
                effort: None,
                perm_mode: crate::domain::PermMode::Default,
                created_at: None,
                is_git: false,
            })
            .await
            .unwrap();
        state
    }

    /// A no-op `HarnessSession` — the RPC tests that use
    /// `state_with_fake_native` only assert on the session row a start call
    /// returns synchronously, before the background startup ever drives a
    /// prompt through this.
    struct FakeSession;
    #[async_trait]
    impl HarnessSession for FakeSession {
        async fn send_prompt(&self, _prompt: TurnPrompt) -> anyhow::Result<()> {
            Ok(())
        }
        async fn cancel(&self) -> anyhow::Result<()> {
            Ok(())
        }
        async fn end(&self) -> anyhow::Result<()> {
            Ok(())
        }
        fn agent_session_id(&self) -> Option<String> {
            None
        }
    }

    struct FakeHarness;
    #[async_trait]
    impl Harness for FakeHarness {
        async fn start_session(&self, _ctx: SessionCtx) -> anyhow::Result<Box<dyn HarnessSession>> {
            Ok(Box::new(FakeSession))
        }
    }

    struct FakeHarnessFactory;
    impl HarnessFactory for FakeHarnessFactory {
        fn create(&self) -> anyhow::Result<Arc<dyn Harness>> {
            Ok(Arc::new(FakeHarness))
        }
    }

    /// Like `state()`, but with a fake `"native"` harness registered and
    /// `HOME`/`XDG_DATA_HOME` redirected into a leaked tempdir. Chat sessions
    /// have no project to carry a harness id, so `start_chat_session` falls
    /// back to the `"native"` runtime — and its background startup touches
    /// `paths::state_dir()` (via `paths::chat_scratch_dir`), which resolves
    /// through `dirs::data_dir()`. Mirrors `control::tests::StateDirGuard`;
    /// since the env override is process-global, every test that uses this
    /// MUST be `#[serial]`.
    pub(crate) async fn state_with_fake_native() -> ApiState {
        let dir = tempfile::tempdir().unwrap();
        std::env::set_var("XDG_DATA_HOME", dir.path().join("data"));
        std::env::set_var("HOME", dir.path());
        std::mem::forget(dir);

        let tmp = tempfile::NamedTempFile::new().unwrap();
        let store = Arc::new(crate::store::Store::open(tmp.path()).await.unwrap());
        prepare_test_agent_persistence(&store).await;
        let mut registries = crate::plugins::Registries::new();
        registries.harness = Arc::new(FakeHarnessFactory);
        let persistence = crate::agents::bootstrap::AgentPersistence::temporary(Arc::clone(&store))
            .await
            .unwrap();
        let cp = crate::control::ControlPlane::new(store, registries, persistence.clone()).await;
        std::mem::forget(tmp);
        ApiState {
            router_server: Arc::new(crate::llm_router::server::RouterServer::new(
                cp.store().clone(),
            )),
            cp,
            agents: persistence.registry,
            agent_knowledge: persistence.knowledge,
            learning_queue: persistence.learning,
            control_token: "t".into(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::tests_support::state;
    use super::{params, ApiError};
    use crate::serve::{serve, ServeOpts};
    use serde_json::json;
    use std::net::Ipv4Addr;

    /// Plaintext-loopback `ServeOpts` for tests that don't exercise TLS.
    fn opts(port: u16) -> ServeOpts {
        ServeOpts {
            addr: Ipv4Addr::LOCALHOST.into(),
            port,
            tls: None,
        }
    }

    /// Exercises the `params` decode helper directly — no command family
    /// wired up yet needs it, so this also keeps it from tripping the
    /// dead-code lint ahead of Task 5+ using it for real.
    #[test]
    fn params_decodes_a_matching_shape() {
        #[derive(serde::Deserialize, PartialEq, Debug)]
        struct Req {
            id: String,
        }
        let decoded: Req = params(json!({ "id": "p1" })).unwrap();
        assert_eq!(
            decoded,
            Req {
                id: "p1".to_string()
            }
        );
    }

    #[test]
    fn params_rejects_a_mismatched_shape_with_bad_request() {
        #[derive(serde::Deserialize, Debug)]
        struct Req {
            #[allow(dead_code)]
            id: String,
        }
        let err: ApiError = params::<Req>(json!({})).unwrap_err();
        assert_eq!(err.status, 400);
        assert!(err.message.contains("bad params"));
    }

    #[test]
    fn conflict_has_http_409_status() {
        let error = ApiError::conflict("at least one main agent must remain");
        assert_eq!(error.status, 409);
        assert_eq!(error.message, "at least one main agent must remain");
    }

    #[test]
    fn agent_registry_errors_map_to_http_statuses() {
        use crate::agents::registry::AgentRegistryError;
        use crate::agents::types::AgentValidationIssue;

        let conflict: ApiError = AgentRegistryError::LastAgent.into();
        assert_eq!(conflict.status, 409);
        assert_eq!(conflict.message, "at least one main agent must remain");

        let missing: ApiError = AgentRegistryError::NotFound("a1".into()).into();
        assert_eq!(missing.status, 404);

        let duplicate: ApiError = AgentRegistryError::DuplicateName("Reviewer".into()).into();
        assert_eq!(duplicate.status, 400);

        let invalid: ApiError = AgentRegistryError::Invalid(vec![AgentValidationIssue {
            field: "name".into(),
            message: "must not be empty".into(),
        }])
        .into();
        assert_eq!(invalid.status, 400);
        assert_eq!(invalid.message, "name: must not be empty");

        let io: ApiError = AgentRegistryError::Io(anyhow::anyhow!("disk full")).into();
        assert_eq!(io.status, 500);
    }

    #[tokio::test]
    async fn rpc_surface_has_no_legacy_orchestration_methods() {
        let port = serve(state().await, opts(0)).await.unwrap();
        let client = reqwest::Client::new();
        for method in [
            "orch_submit",
            "orch_list_roots",
            "orch_tasks",
            "orch_cancel",
            "orch_retry",
            "orch_answer_block",
            "orch_steer",
        ] {
            let response = client
                .post(format!("http://127.0.0.1:{port}/rpc/{method}"))
                .bearer_auth("t")
                .json(&json!({}))
                .send()
                .await
                .unwrap();
            assert_eq!(
                response.status(),
                reqwest::StatusCode::NOT_FOUND,
                "{method}"
            );
            let body: serde_json::Value = response.json().await.unwrap();
            assert_eq!(
                body["error"],
                format!("unknown method: {method}"),
                "{method}"
            );
        }
    }

    #[tokio::test]
    async fn unknown_method_is_404_with_error_envelope() {
        let port = serve(state().await, opts(0)).await.unwrap();
        let r = reqwest::Client::new()
            .post(format!("http://127.0.0.1:{port}/rpc/nope"))
            .bearer_auth("t")
            .json(&json!({}))
            .send()
            .await
            .unwrap();
        assert_eq!(r.status(), reqwest::StatusCode::NOT_FOUND);
        let body: serde_json::Value = r.json().await.unwrap();
        assert_eq!(body["error"], "unknown method: nope");
    }

    #[tokio::test]
    async fn list_projects_dispatches_empty() {
        let port = serve(state().await, opts(0)).await.unwrap();
        let r = reqwest::Client::new()
            .post(format!("http://127.0.0.1:{port}/rpc/list_projects"))
            .bearer_auth("t")
            .json(&json!({}))
            .send()
            .await
            .unwrap();
        assert_eq!(r.status(), reqwest::StatusCode::OK);
        let body: serde_json::Value = r.json().await.unwrap();
        assert_eq!(body, json!([]));
    }

    #[tokio::test]
    async fn approvals_endpoint_resolves_a_registered_approval() {
        let s = state().await;
        let rx = s.cp.approvals_for_test_register("run-9", "req-9");
        let port = serve(s, opts(0)).await.unwrap();
        let r = reqwest::Client::new()
            .post(format!("http://127.0.0.1:{port}/approvals/run-9/req-9"))
            .bearer_auth("t")
            .json(
                &json!({ "response": { "decision": "allowOnce", "scope": null, "payload": null } }),
            )
            .send()
            .await
            .unwrap();
        assert_eq!(r.status(), reqwest::StatusCode::OK);
        assert!(rx.await.unwrap().allowed());
    }

    #[tokio::test]
    async fn list_audit_returns_recorded_rows() {
        let s = state().await;
        s.cp.store()
            .record_audit(
                crate::domain::WriteOrigin::Agent,
                Some("s"),
                "app_jobs",
                "create",
                "allow",
            )
            .await
            .unwrap();
        let port = serve(s, opts(0)).await.unwrap();
        let resp: serde_json::Value = reqwest::Client::new()
            .post(format!("http://127.0.0.1:{port}/rpc/list_audit"))
            .bearer_auth("t")
            .json(&json!({"limit": 10}))
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        assert_eq!(resp[0]["tool"], "app_jobs");
    }
}
