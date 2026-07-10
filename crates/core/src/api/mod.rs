//! The engine's callable command surface: every operation Cockpit's Tauri
//! layer proxies lands here as `dispatch(state, method, params)`. Method
//! names match the Tauri command names 1:1; params objects use the Rust
//! snake_case parameter names. One submodule per command family.

pub mod apps_api;
pub mod endpoint_api;
pub mod fsview_api;
pub mod gateways_api;
pub mod native_api;
pub mod runtimes_api;
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
        m if session_io_api::HANDLES.contains(&m) => session_io_api::dispatch(state, m, p).await,
        m if fsview_api::HANDLES.contains(&m) => fsview_api::dispatch(state, m, p).await,
        m if skills_api::HANDLES.contains(&m) => skills_api::dispatch(state, m, p).await,
        m if runtimes_api::HANDLES.contains(&m) => runtimes_api::dispatch(state, m, p).await,
        m if endpoint_api::HANDLES.contains(&m) => endpoint_api::dispatch(state, m, p).await,
        _ => Err(ApiError::not_found(format!("unknown method: {method}"))),
    }
}

/// Shared test scaffolding for this module and every later command-family
/// module under `api/`. `pub(crate)` so sibling `api::*` test modules can
/// build an `ApiState` without duplicating the boilerplate.
#[cfg(test)]
pub(crate) mod tests_support {
    use crate::serve::ApiState;
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
            token: Some("t".into()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::tests_support::state;
    use super::{params, ApiError};
    use crate::serve::serve;
    use serde_json::json;

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
        let port = serve(state().await, 0).await.unwrap();
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
        let port = serve(state().await, 0).await.unwrap();
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
        let port = serve(s, 0).await.unwrap();
        let r = reqwest::Client::new()
            .post(format!("http://127.0.0.1:{port}/approvals/req-9"))
            .bearer_auth("t")
            .json(&json!({ "allow": true }))
            .send()
            .await
            .unwrap();
        assert_eq!(r.status(), reqwest::StatusCode::OK);
        assert!(rx.await.unwrap());
    }
}
