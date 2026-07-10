//! Skills screen commands: list/install/remove/refresh git-backed native
//! skills and plugin-bundled skill packs. Moved verbatim (per the Move
//! Recipe) from `apps/cockpit/src-tauri/src/skills_cmd.rs`; that file keeps
//! its own copy until the proxy rewrite in Tasks 15-16.

use super::{ok, params, ApiError};
use crate::serve::ApiState;
use serde::Deserialize;
use serde_json::Value;

pub(crate) const HANDLES: &[&str] = &[
    "list_skills",
    "install_skill",
    "remove_skill",
    "refresh_skill",
];

#[derive(Deserialize)]
struct SourceP {
    source: String,
}
#[derive(Deserialize)]
struct IdP {
    id: String,
}

pub(crate) async fn dispatch(_state: &ApiState, method: &str, p: Value) -> Result<Value, ApiError> {
    match method {
        "list_skills" => ok(
            crate::skills_install::list_installed_skills().map_err(|message| ApiError {
                status: 500,
                message: message.to_string(),
            })?,
        ),
        "install_skill" => {
            let a: SourceP = params(p)?;
            ok(crate::skills_install::install_skill_source(&a.source)
                .await
                .map_err(|message| ApiError {
                    status: 500,
                    message: message.to_string(),
                })?)
        }
        "remove_skill" => {
            let a: IdP = params(p)?;
            ok(
                crate::skills_install::remove_installed_skill(&a.id).map_err(|message| {
                    ApiError {
                        status: 500,
                        message: message.to_string(),
                    }
                })?,
            )
        }
        "refresh_skill" => {
            let a: IdP = params(p)?;
            ok(crate::skills_install::refresh_installed_skill(&a.id)
                .await
                .map_err(|message| ApiError {
                    status: 500,
                    message: message.to_string(),
                })?)
        }
        _ => Err(ApiError::not_found(format!("unknown method: {method}"))),
    }
}

#[cfg(test)]
mod tests {
    use crate::api::{dispatch, tests_support::state};
    use serde_json::json;

    #[tokio::test]
    async fn list_skills_dispatches_and_decodes_as_an_array() {
        let s = state().await;
        let out = dispatch(&s, "list_skills", json!({})).await.unwrap();
        assert!(
            out.is_array(),
            "expected list_skills to decode as an array, got: {out}"
        );
    }
}
