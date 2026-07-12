//! Read-only audit endpoint for the Cockpit Settings → Audit feed. Task 7's
//! `AppControl` facade writes one `audit` row per app-control mutation
//! (`Store::record_audit`); this family only reads them back
//! (`Store::list_audit`) — no mutation lands here.

use super::{ok, params, ApiError};
use crate::serve::ApiState;
use serde::Deserialize;
use serde_json::Value;

pub(crate) const HANDLES: &[&str] = &["list_audit"];

#[derive(Deserialize)]
struct ListAuditP {
    #[serde(default = "default_limit")]
    limit: u32,
}

fn default_limit() -> u32 {
    100
}

pub(crate) async fn dispatch(state: &ApiState, method: &str, p: Value) -> Result<Value, ApiError> {
    match method {
        "list_audit" => {
            let a: ListAuditP = params(p)?;
            ok(state.cp.store().list_audit(a.limit).await?)
        }
        _ => Err(ApiError::not_found(format!("unknown method: {method}"))),
    }
}

#[cfg(test)]
mod tests {
    use crate::api::tests_support;
    use serde_json::json;

    #[tokio::test]
    async fn list_audit_dispatches_and_decodes_recorded_rows() {
        let s = tests_support::state().await;
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

        let out = crate::api::dispatch(&s, "list_audit", json!({ "limit": 10 }))
            .await
            .unwrap();
        let rows = out.as_array().unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0]["tool"], "app_jobs");
        assert_eq!(rows[0]["action"], "create");
    }

    #[tokio::test]
    async fn list_audit_defaults_the_limit_when_omitted() {
        let s = tests_support::state().await;
        for i in 0..3 {
            s.cp.store()
                .record_audit(
                    crate::domain::WriteOrigin::User,
                    None,
                    "app_projects",
                    "update",
                    "allow",
                )
                .await
                .unwrap();
            let _ = i;
        }

        let out = crate::api::dispatch(&s, "list_audit", json!({}))
            .await
            .unwrap();
        assert_eq!(out.as_array().unwrap().len(), 3);
    }
}
