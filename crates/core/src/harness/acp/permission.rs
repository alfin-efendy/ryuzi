//! Answer-by-kind mapper: translates an `ApprovalDecision` into a
//! `RequestPermissionResponse` by finding the matching `PermissionOptionKind`
//! in the request's offered options.
//!
//! Task 4 / Spec 3A. The live (3A) decision path is binary (AllowOnce or
//! RejectOnce from the hub's bool), but this module handles all five
//! `ApprovalDecision` variants so that Spec 3B can reuse `map_response`
//! without modification.

use agent_client_protocol::schema::v1::{
    PermissionOption, PermissionOptionKind, RequestPermissionOutcome, RequestPermissionRequest,
    RequestPermissionResponse, SelectedPermissionOutcome,
};

use crate::domain::ApprovalDecision;

/// Find the `option_id` (as a `String`) of the first option in `options` whose
/// `kind` matches `kind`. Returns `None` if no matching option exists.
pub fn find_option(options: &[PermissionOption], kind: PermissionOptionKind) -> Option<String> {
    options
        .iter()
        .find(|opt| opt.kind == kind)
        .map(|opt| opt.option_id.0.to_string())
}

/// Map an `ApprovalDecision` to a `RequestPermissionResponse` by locating the
/// matching `PermissionOptionKind` in the request's offered options.
///
/// Fallback rules (so the client never invents an option_id):
/// - `AllowAlways` → AllowAlways, then AllowOnce
/// - `AllowOnce`   → AllowOnce, then AllowAlways
/// - `RejectAlways`→ RejectAlways, then RejectOnce
/// - `RejectOnce`  → RejectOnce, then RejectAlways
/// - `Cancel`      → always `Cancelled` (no option selected)
///
/// If no matching option is found, returns `Cancelled`.
pub fn map_response(
    request: &RequestPermissionRequest,
    decision: ApprovalDecision,
) -> RequestPermissionResponse {
    let selected_id = match decision {
        ApprovalDecision::AllowAlways => {
            find_option(&request.options, PermissionOptionKind::AllowAlways)
                .or_else(|| find_option(&request.options, PermissionOptionKind::AllowOnce))
        }
        ApprovalDecision::AllowOnce => {
            find_option(&request.options, PermissionOptionKind::AllowOnce)
                .or_else(|| find_option(&request.options, PermissionOptionKind::AllowAlways))
        }
        ApprovalDecision::RejectAlways => {
            find_option(&request.options, PermissionOptionKind::RejectAlways)
                .or_else(|| find_option(&request.options, PermissionOptionKind::RejectOnce))
        }
        ApprovalDecision::RejectOnce => {
            find_option(&request.options, PermissionOptionKind::RejectOnce)
                .or_else(|| find_option(&request.options, PermissionOptionKind::RejectAlways))
        }
        ApprovalDecision::Cancel => None,
    };

    if let Some(option_id) = selected_id {
        RequestPermissionResponse::new(RequestPermissionOutcome::Selected(
            SelectedPermissionOutcome::new(option_id),
        ))
    } else {
        RequestPermissionResponse::new(RequestPermissionOutcome::Cancelled)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn map_response_selects_option_by_kind() {
        let req = crate::harness::acp::testkit::perm_request_with_kinds();
        let resp = map_response(&req, crate::domain::ApprovalDecision::AllowOnce);
        // outcome is Selected with the AllowOnce option's id
        assert!(
            crate::harness::acp::testkit::is_selected_allow_once(&resp),
            "expected Selected(allow_once), got: {:?}",
            resp.outcome
        );

        let cancel = map_response(&req, crate::domain::ApprovalDecision::Cancel);
        assert!(
            crate::harness::acp::testkit::is_cancelled(&cancel),
            "expected Cancelled, got: {:?}",
            cancel.outcome
        );
    }
}
