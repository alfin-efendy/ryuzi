use crate::llm_router::model_effort::TurnEffortPolicy;
use serde_json::Value;
use std::sync::Arc;
use tokio::sync::mpsc;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RouteFailureCategory {
    Unavailable,
    Authentication,
    Quota,
    RateLimit,
    Transport,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RouteSelectionReason {
    Initial,
    Ordered,
    RoundRobin,
    Failover(RouteFailureCategory),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RouteSelection {
    pub requested_model: String,
    pub resolved_provider_id: String,
    pub resolved_family: String,
    pub resolved_model: String,
    pub resolved_model_display_name: String,
    pub effective_effort: Option<String>,
    pub effective_effort_label: Option<String>,
    pub connection_id: String,
    pub connection_label: String,
    pub reason: RouteSelectionReason,
}

pub type AnthropicEvent = (String, Value);

pub struct RoutedStream {
    pub selection: RouteSelection,
    pub events: mpsc::Receiver<anyhow::Result<AnthropicEvent>>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RouteObservationContext {
    pub session_pk: String,
}

#[derive(Clone)]
pub struct LlmRequestMetadata {
    pub effort_policy: Arc<TurnEffortPolicy>,
    pub observation: Option<RouteObservationContext>,
}

pub struct LlmRequest {
    pub body: Value,
    pub metadata: LlmRequestMetadata,
}

#[derive(Debug, PartialEq, Eq)]
struct RouteIdentity<'a> {
    resolved_provider_id: &'a str,
    resolved_family: &'a str,
    resolved_model: &'a str,
    effective_effort: Option<&'a str>,
    connection_id: &'a str,
}

impl<'a> From<&'a RouteSelection> for RouteIdentity<'a> {
    fn from(selection: &'a RouteSelection) -> Self {
        Self {
            resolved_provider_id: &selection.resolved_provider_id,
            resolved_family: &selection.resolved_family,
            resolved_model: &selection.resolved_model,
            effective_effort: selection.effective_effort.as_deref(),
            connection_id: &selection.connection_id,
        }
    }
}

pub(crate) fn classify_failure(
    status: Option<u16>,
    transport: bool,
    raw_message: &str,
) -> RouteFailureCategory {
    if transport {
        return RouteFailureCategory::Transport;
    }
    if matches!(status, Some(401 | 403)) {
        return RouteFailureCategory::Authentication;
    }

    let message = raw_message.to_ascii_lowercase();
    if ["rate limit", "rate_limit", "rate-limit"]
        .iter()
        .any(|needle| message.contains(needle))
    {
        return RouteFailureCategory::RateLimit;
    }
    if ["quota", "usage", "insufficient", "exceeded"]
        .iter()
        .any(|needle| message.contains(needle))
    {
        return RouteFailureCategory::Quota;
    }
    if [
        "authentication",
        "unauthorized",
        "forbidden",
        "expired",
        "reconnect",
    ]
    .iter()
    .any(|needle| message.contains(needle))
    {
        return RouteFailureCategory::Authentication;
    }
    if status == Some(429) {
        return RouteFailureCategory::RateLimit;
    }
    RouteFailureCategory::Unavailable
}

pub fn notice_text(previous: Option<&RouteSelection>, current: &RouteSelection) -> Option<String> {
    let previous = previous?;
    if RouteIdentity::from(previous) == RouteIdentity::from(current) {
        return None;
    }

    let model_changed = previous.resolved_family != current.resolved_family
        || previous.resolved_model != current.resolved_model
        || previous.effective_effort != current.effective_effort;
    let account_changed = previous.resolved_provider_id != current.resolved_provider_id
        || previous.connection_id != current.connection_id;
    let include_account_suffix = account_changed
        && matches!(
            current.reason,
            RouteSelectionReason::Ordered
                | RouteSelectionReason::RoundRobin
                | RouteSelectionReason::Failover(_)
        );

    let mut copy = if model_changed {
        let mut copy = format!("Switched to {}", current.resolved_model_display_name);
        if let Some(effort_label) = &current.effective_effort_label {
            copy.push_str(" · ");
            copy.push_str(effort_label);
        }
        if include_account_suffix {
            copy.push_str(" via ");
            copy.push_str(&current.connection_label);
        }
        copy
    } else {
        format!("Account switched to {}", current.connection_label)
    };

    if include_account_suffix {
        if let Some(reason) = reason_text(&current.reason) {
            copy.push_str(" · ");
            copy.push_str(reason);
        }
    }
    Some(copy)
}

fn reason_text(reason: &RouteSelectionReason) -> Option<&'static str> {
    match reason {
        RouteSelectionReason::Initial => None,
        RouteSelectionReason::Ordered => Some("account order"),
        RouteSelectionReason::RoundRobin => Some("round robin"),
        RouteSelectionReason::Failover(category) => Some(match category {
            RouteFailureCategory::Unavailable => "provider unavailable",
            RouteFailureCategory::Authentication => "authentication unavailable",
            RouteFailureCategory::Quota => "quota unavailable",
            RouteFailureCategory::RateLimit => "rate limit",
            RouteFailureCategory::Transport => "transport unavailable",
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn selection() -> RouteSelection {
        RouteSelection {
            requested_model: "sol".into(),
            resolved_provider_id: "provider-a".into(),
            resolved_family: "openai".into(),
            resolved_model: "gpt-5.6-sol".into(),
            resolved_model_display_name: "5.6 Sol".into(),
            effective_effort: Some("high".into()),
            effective_effort_label: Some("High".into()),
            connection_id: "connection-a".into(),
            connection_label: "Personal Codex".into(),
            reason: RouteSelectionReason::Initial,
        }
    }

    #[test]
    fn first_selection_has_no_notice() {
        assert_eq!(notice_text(None, &selection()), None);
    }

    #[test]
    fn identity_ignores_mutable_labels_and_reason() {
        let previous = selection();
        let mut current = previous.clone();
        current.requested_model = "friendly-alias".into();
        current.resolved_model_display_name = "Renamed Sol".into();
        current.effective_effort_label = Some("Maximum".into());
        current.connection_label = "Renamed account".into();
        current.reason = RouteSelectionReason::Failover(RouteFailureCategory::Quota);

        assert_eq!(notice_text(Some(&previous), &current), None);
    }

    #[test]
    fn formats_model_effort_switch() {
        let previous = selection();
        let mut current = previous.clone();
        current.resolved_model = "gpt-5.6-sol-plus".into();
        current.resolved_model_display_name = "5.6 Sol".into();
        current.effective_effort = Some("ultra".into());
        current.effective_effort_label = Some("Ultra".into());

        assert_eq!(
            notice_text(Some(&previous), &current).as_deref(),
            Some("Switched to 5.6 Sol · Ultra")
        );
    }

    #[test]
    fn formats_account_round_robin_switch() {
        let previous = selection();
        let mut current = previous.clone();
        current.connection_id = "connection-work".into();
        current.connection_label = "Work Codex".into();
        current.reason = RouteSelectionReason::RoundRobin;

        assert_eq!(
            notice_text(Some(&previous), &current).as_deref(),
            Some("Account switched to Work Codex · round robin")
        );
    }

    #[test]
    fn formats_quota_failover_switch() {
        let previous = selection();
        let mut current = previous.clone();
        current.connection_id = "connection-backup".into();
        current.connection_label = "Backup Codex".into();
        current.reason = RouteSelectionReason::Failover(RouteFailureCategory::Quota);

        assert_eq!(
            notice_text(Some(&previous), &current).as_deref(),
            Some("Account switched to Backup Codex · quota unavailable")
        );
    }

    #[test]
    fn formats_combined_switch_with_safe_reason() {
        let previous = selection();
        let mut current = previous.clone();
        current.resolved_model = "claude-opus-4-1".into();
        current.resolved_model_display_name = "Opus 4.1".into();
        current.effective_effort = None;
        current.effective_effort_label = None;
        current.connection_id = "connection-backup".into();
        current.connection_label = "Backup Claude".into();
        current.reason = RouteSelectionReason::Failover(RouteFailureCategory::Authentication);

        assert_eq!(
            notice_text(Some(&previous), &current).as_deref(),
            Some("Switched to Opus 4.1 via Backup Claude · authentication unavailable")
        );
    }

    #[test]
    fn formats_combined_initial_without_account_suffix() {
        let previous = selection();
        let mut current = previous.clone();
        current.resolved_model = "claude-opus-4-1".into();
        current.resolved_model_display_name = "Opus 4.1".into();
        current.effective_effort = Some("ultra".into());
        current.effective_effort_label = Some("Ultra".into());
        current.connection_id = "connection-backup".into();
        current.connection_label = "Backup Claude".into();
        current.reason = RouteSelectionReason::Initial;

        assert_eq!(
            notice_text(Some(&previous), &current).as_deref(),
            Some("Switched to Opus 4.1 · Ultra")
        );
    }

    #[test]
    fn provider_or_connection_change_is_account_change() {
        let previous = selection();
        for (provider, connection, label) in [
            ("provider-b", "connection-a", "Provider account"),
            ("provider-a", "connection-b", "Other account"),
        ] {
            let mut current = previous.clone();
            current.resolved_provider_id = provider.into();
            current.connection_id = connection.into();
            current.connection_label = label.into();

            assert_eq!(
                notice_text(Some(&previous), &current).as_deref(),
                Some(format!("Account switched to {label}").as_str())
            );
        }
    }

    #[test]
    fn failure_classification_distinguishes_auth_quota_rate_transport_and_unavailable() {
        assert_eq!(
            classify_failure(Some(401), false, "unauthorized"),
            RouteFailureCategory::Authentication
        );
        assert_eq!(
            classify_failure(Some(429), false, "usage quota exceeded"),
            RouteFailureCategory::Quota
        );
        assert_eq!(
            classify_failure(Some(429), false, "rate limit reached"),
            RouteFailureCategory::RateLimit
        );
        assert_eq!(
            classify_failure(None, true, "connection refused"),
            RouteFailureCategory::Transport
        );
        assert_eq!(
            classify_failure(Some(503), false, "temporarily overloaded"),
            RouteFailureCategory::Unavailable
        );
    }

    #[test]
    fn failure_classification_never_copies_raw_messages() {
        let secret = "quota exhausted for sk-secret-token";
        let mut current = selection();
        current.connection_id = "connection-backup".into();
        current.connection_label = "Backup Codex".into();
        current.reason = RouteSelectionReason::Failover(classify_failure(Some(429), false, secret));

        let copy = notice_text(Some(&selection()), &current).unwrap();
        assert_eq!(copy, "Account switched to Backup Codex · quota unavailable");
        assert!(!copy.contains(secret));
        assert!(!copy.contains("sk-secret-token"));
    }
}
