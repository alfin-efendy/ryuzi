//! The engine's callable command surface: every operation Cockpit's Tauri
//! layer proxies lands here as `dispatch(state, method, params)`. Method
//! names match the Tauri command names 1:1; params objects use the Rust
//! snake_case parameter names. One submodule per command family.

pub mod agent_api;
pub mod apps_api;
pub mod audit;
pub mod connections_api;
pub mod endpoint_api;
pub mod extension_status_api;
pub mod fsview_api;
pub mod gateways_api;
pub mod learning_api;
pub mod native_api;
pub mod orch_api;
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
}

impl From<anyhow::Error> for ApiError {
    fn from(e: anyhow::Error) -> Self {
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
        m if gateways_api::HANDLES.contains(&m) => gateways_api::dispatch(state, m, p).await,
        m if apps_api::HANDLES.contains(&m) => apps_api::dispatch(state, m, p).await,
        m if native_api::HANDLES.contains(&m) => native_api::dispatch(state, m, p).await,
        m if agent_api::HANDLES.contains(&m) => agent_api::dispatch(state, m, p).await,
        m if session_io_api::HANDLES.contains(&m) => session_io_api::dispatch(state, m, p).await,
        m if fsview_api::HANDLES.contains(&m) => fsview_api::dispatch(state, m, p).await,
        m if skills_api::HANDLES.contains(&m) => skills_api::dispatch(state, m, p).await,
        m if endpoint_api::HANDLES.contains(&m) => endpoint_api::dispatch(state, m, p).await,
        m if connections_api::HANDLES.contains(&m) => connections_api::dispatch(state, m, p).await,
        m if plugins_api::HANDLES.contains(&m) => plugins_api::dispatch(state, m, p).await,
        m if learning_api::HANDLES.contains(&m) => learning_api::dispatch(state, m, p).await,
        m if orch_api::HANDLES.contains(&m) => orch_api::dispatch(state, m, p).await,
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

    /// A fresh in-memory-backed `ApiState` with a real bearer token ("t").
    /// Leaks the backing tempfile's guard for test simplicity — acceptable
    /// since each test process exits shortly after.
    pub(crate) async fn state() -> ApiState {
        let tmp = tempfile::NamedTempFile::new().unwrap();
        let store = crate::store::Store::open(tmp.path()).await.unwrap();
        let cp = crate::control::ControlPlane::new(store, crate::plugins::Registries::new()).await;
        std::mem::forget(tmp);
        ApiState {
            router_server: Arc::new(crate::llm_router::server::RouterServer::new(
                cp.store().clone(),
            )),
            cp,
            control_token: "t".into(),
        }
    }

    /// Like `state()`, but with one connected project (`"p1"`) already in the
    /// store — for command families (orch, scheduler, ...) whose calls need a
    /// real `project_id` to validate against.
    pub(crate) async fn state_with_project() -> ApiState {
        let s = state().await;
        s.cp.store()
            .insert_project(crate::domain::Project {
                project_id: "p1".into(),
                name: "demo".into(),
                workdir: "/tmp/demo".into(),
                source: None,
                model: None,
                effort: None,
                perm_mode: crate::domain::PermMode::Default,
                created_at: None,
                is_git: false,
            })
            .await
            .unwrap();
        s
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
        let store = crate::store::Store::open(tmp.path()).await.unwrap();
        let mut registries = crate::plugins::Registries::new();
        registries.harness = Arc::new(FakeHarnessFactory);
        let cp = crate::control::ControlPlane::new(store, registries).await;
        std::mem::forget(tmp);
        ApiState {
            router_server: Arc::new(crate::llm_router::server::RouterServer::new(
                cp.store().clone(),
            )),
            cp,
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
        let rx = s.cp.approvals_for_test_register("req-9");
        let port = serve(s, opts(0)).await.unwrap();
        let r = reqwest::Client::new()
            .post(format!("http://127.0.0.1:{port}/approvals/req-9"))
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
        let port = serve(s, 0).await.unwrap();
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
