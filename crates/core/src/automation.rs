//! Persisted automation Hook configuration and dispatch history.

use crate::llm_router::secrets;
use crate::paths::{new_id, now_ms};
use crate::store::Store;
use anyhow::{bail, Context};
use futures::FutureExt;
use rusqlite::{params, OptionalExtension, Row};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use specta::Type;
use std::collections::VecDeque;
use std::net::{IpAddr, Ipv4Addr};
use std::str::FromStr;
use std::sync::{Arc, OnceLock};
use uuid::Uuid;

const MAX_RUNS: u32 = 20;
const MAX_DETAIL_ATTEMPTS: u32 = 3;
const MAX_TOOL_VALUE_BYTES: usize = 64 * 1024;
pub const MAX_ENVELOPE_BYTES: usize = 256 * 1024;

const MAX_OUTBOUND_TEMPLATE_BYTES: usize = 64 * 1024;
const OUTBOUND_RETRY_DELAYS: [std::time::Duration; 3] = [
    std::time::Duration::from_secs(1),
    std::time::Duration::from_secs(5),
    std::time::Duration::from_secs(30),
];
const TRUNCATED_SUFFIX: &str = "…[truncated]";

/// Render the stable default delivery envelope, or replace the two permitted
/// whole-string template values with their JSON values. This deliberately never
/// interpolates JSON into strings, so event data cannot alter template syntax.
pub fn render_outbound_payload(
    template: Option<&str>,
    event: &Value,
    run: &Value,
) -> anyhow::Result<Value> {
    let Some(template) = template else {
        return Ok(event.clone());
    };
    if template.len() > MAX_OUTBOUND_TEMPLATE_BYTES {
        bail!("outbound payload template must be at most 65536 bytes");
    }
    let mut payload: Value =
        serde_json::from_str(template).context("outbound payload template must be valid JSON")?;
    replace_payload_placeholders(&mut payload, event, run)?;
    if serde_json::to_vec(&payload)
        .context("could not serialize outbound payload")?
        .len()
        > MAX_ENVELOPE_BYTES
    {
        bail!("rendered outbound payload must be at most 262144 bytes");
    }
    Ok(payload)
}

fn replace_payload_placeholders(
    value: &mut Value,
    event: &Value,
    run: &Value,
) -> anyhow::Result<()> {
    match value {
        Value::String(placeholder) => match placeholder.as_str() {
            "${event}" => *value = event.clone(),
            "${run}" => *value = run.clone(),
            text if text.contains("${") => bail!(
                "outbound payload placeholders must be exactly ${{event}} or ${{run}} JSON values"
            ),
            _ => {}
        },
        Value::Array(values) => {
            for value in values {
                replace_payload_placeholders(value, event, run)?;
            }
        }
        Value::Object(values) => {
            for value in values.values_mut() {
                replace_payload_placeholders(value, event, run)?;
            }
        }
        _ => {}
    }
    Ok(())
}

/// Stable, untrusted event input for automation dispatch. `event`, `source`,
/// and `data` deliberately remain the only externally visible shape so the
/// outbound webhook delivery added later can reuse this exact payload.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AutomationEnvelope {
    pub event: TriggerKind,
    pub occurred_at: String,
    pub source: AutomationSource,
    pub data: Value,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AutomationSource {
    pub kind: String,
    pub id: String,
}

impl AutomationSource {
    pub fn new(kind: impl Into<String>, id: impl Into<String>) -> Self {
        Self {
            kind: kind.into(),
            id: id.into(),
        }
    }
}

impl AutomationEnvelope {
    pub fn new(
        event: TriggerKind,
        occurred_at: impl Into<String>,
        source: AutomationSource,
        data: Value,
    ) -> Self {
        Self {
            event,
            occurred_at: occurred_at.into(),
            source,
            data,
        }
    }

    /// Cap an envelope before it reaches persistence, a prompt, or an eventual
    /// outbound delivery. Object keys are considered lexically, making the
    /// retained prefix deterministic across runs.
    pub fn capped(mut self) -> Self {
        cap_tool_values(&mut self.data);
        self.occurred_at = cap_metadata_string(self.occurred_at);
        self.source.kind = cap_metadata_string(self.source.kind);
        self.source.id = cap_metadata_string(self.source.id);
        while serde_json::to_vec(&self).map_or(true, |bytes| bytes.len() > MAX_ENVELOPE_BYTES) {
            let before = serde_json::to_vec(&self.data).map_or(usize::MAX, |bytes| bytes.len());
            self.data = shrink_json(self.data);
            if serde_json::to_vec(&self.data).is_ok_and(|bytes| bytes.len() < before) {
                continue;
            }
            if shrink_metadata_string(&mut self.occurred_at)
                || shrink_metadata_string(&mut self.source.kind)
                || shrink_metadata_string(&mut self.source.id)
            {
                continue;
            }
            self.data = Value::Null;
            break;
        }
        self
    }
}

fn shrink_metadata_string(value: &mut String) -> bool {
    if value.is_empty() {
        return false;
    }
    let next = if value.len() <= TRUNCATED_SUFFIX.len() {
        String::new()
    } else {
        truncate_string(value, value.len().saturating_div(2))
            .as_str()
            .unwrap_or_default()
            .to_string()
    };
    if next == *value {
        return false;
    }
    *value = next;
    true
}

fn cap_metadata_string(value: String) -> String {
    let max_bytes = MAX_ENVELOPE_BYTES
        .saturating_sub(serde_json::to_vec(&TriggerKind::WebhookInbound).map_or(0, |v| v.len()))
        .saturating_div(4);
    truncate_string(&value, max_bytes)
        .as_str()
        .unwrap_or_default()
        .to_string()
}
fn cap_tool_values(value: &mut Value) {
    let Value::Object(data) = value else { return };
    for key in ["input", "result"] {
        if let Some(value) = data.remove(key) {
            data.insert(key.to_string(), cap_json(value, MAX_TOOL_VALUE_BYTES));
        }
    }
}

fn cap_json(value: Value, max_bytes: usize) -> Value {
    match value {
        Value::String(value) => truncate_string(&value, max_bytes.saturating_div(2)),
        Value::Array(values) => {
            let mut out = Vec::new();
            for value in values {
                let value = cap_json(value, max_bytes);
                if serde_json::to_vec(&out).map_or(usize::MAX, |v| v.len()) >= max_bytes {
                    break;
                }
                out.push(value);
            }
            Value::Array(out)
        }
        Value::Object(values) => {
            let mut out = serde_json::Map::new();
            let mut keys: Vec<_> = values.into_iter().collect();
            keys.sort_by(|left, right| left.0.cmp(&right.0));
            for (key, value) in keys {
                let value = cap_json(value, max_bytes);
                out.insert(key.clone(), value);
                if serde_json::to_vec(&out).map_or(usize::MAX, |v| v.len()) > max_bytes {
                    out.remove(&key);
                    break;
                }
            }
            Value::Object(out)
        }
        value => value,
    }
}

fn shrink_json(value: Value) -> Value {
    match value {
        Value::String(value) => truncate_string(&value, value.len().saturating_div(2)),
        Value::Array(mut values) => {
            values.pop();
            Value::Array(values)
        }
        Value::Object(mut values) => {
            let Some(key) = values.keys().max().cloned() else {
                return Value::String(TRUNCATED_SUFFIX.to_string());
            };
            values.remove(&key);
            Value::Object(values)
        }
        _ => Value::String(TRUNCATED_SUFFIX.to_string()),
    }
}

fn truncate_string(value: &str, max_bytes: usize) -> Value {
    if value.len() <= max_bytes {
        return Value::String(value.to_string());
    }
    let budget = max_bytes.saturating_sub(TRUNCATED_SUFFIX.len());
    let mut end = budget.min(value.len());
    while end > 0 && !value.is_char_boundary(end) {
        end -= 1;
    }
    Value::String(format!("{}{}", &value[..end], TRUNCATED_SUFFIX))
}

/// Immutable provenance for hook-created sessions. It is persisted separately
/// from the general session shape to avoid treating external event data as a
/// user-editable session attribute.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HookOrigin {
    pub kind: String,
    pub hook_id: String,
    pub run_id: String,
    pub depth: u8,
}

impl HookOrigin {
    pub fn new(hook_id: impl Into<String>, run_id: impl Into<String>, depth: u8) -> Self {
        Self {
            kind: "hook".into(),
            hook_id: hook_id.into(),
            run_id: run_id.into(),
            depth,
        }
    }

    pub const fn allows_dispatch(&self) -> bool {
        self.depth < 3
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RateVerdict {
    Accepted,
    HookLimited,
    EngineLimited,
    DepthLimited,
}

#[async_trait::async_trait]
pub trait AutomationEventSink: Send + Sync {
    /// Observe a native lifecycle event after the script/extension hook sinks
    /// have run. Implementations must not affect the native hook outcome.
    async fn observe_lifecycle(&self, trigger: TriggerKind, session_pk: String, data: Value);
}

/// Schedule an observational lifecycle dispatch after the native hook result is
/// known. A bounded queue keeps lifecycle persistence/agent starts out of the
/// session and tool paths; a dropped task panic is observed and logged.
pub fn dispatch_lifecycle_observation(
    sink: Option<Arc<dyn AutomationEventSink>>,
    trigger: TriggerKind,
    session_pk: String,
    data: Value,
) {
    let Some(sink) = sink else { return };
    lifecycle_dispatcher().dispatch(sink, trigger, session_pk, data);
}

struct LifecycleDispatch {
    sender: tokio::sync::mpsc::Sender<LifecycleObservation>,
}

struct LifecycleObservation {
    sink: Arc<dyn AutomationEventSink>,
    trigger: TriggerKind,
    session_pk: String,
    data: Value,
}

impl LifecycleDispatch {
    fn dispatch(
        &self,
        sink: Arc<dyn AutomationEventSink>,
        trigger: TriggerKind,
        session_pk: String,
        data: Value,
    ) {
        if self
            .sender
            .try_send(LifecycleObservation {
                sink,
                trigger,
                session_pk,
                data,
            })
            .is_err()
        {
            tracing::warn!("automation lifecycle observation dropped because the queue is full");
        }
    }
}

fn lifecycle_dispatcher() -> &'static LifecycleDispatch {
    static DISPATCHER: OnceLock<LifecycleDispatch> = OnceLock::new();
    DISPATCHER.get_or_init(|| {
        let (sender, mut receiver) = tokio::sync::mpsc::channel(256);
        tokio::spawn(async move {
            while let Some(observation) = receiver.recv().await {
                let LifecycleObservation {
                    sink,
                    trigger,
                    session_pk,
                    data,
                } = observation;
                if let Err(error) =
                    std::panic::AssertUnwindSafe(sink.observe_lifecycle(trigger, session_pk, data))
                        .catch_unwind()
                        .await
                {
                    tracing::error!(?error, "automation lifecycle observer panicked");
                }
            }
        });
        LifecycleDispatch { sender }
    })
}

/// Process-local rolling-window limiter. Runs themselves are persisted before
/// execution, so a verdict of anything other than `Accepted` has durable
/// history without pretending an action ran.
pub struct Dispatcher {
    accepted: std::sync::Mutex<VecDeque<(std::time::Instant, String, TriggerKind)>>,
}

impl Dispatcher {
    pub fn new() -> Self {
        Self {
            accepted: std::sync::Mutex::new(VecDeque::new()),
        }
    }

    pub fn rate_verdict(&self, hook_id: &str, trigger: TriggerKind, depth: u8) -> RateVerdict {
        if depth >= 3 {
            return RateVerdict::DepthLimited;
        }
        let now = std::time::Instant::now();
        let mut accepted = self.accepted.lock().unwrap();
        accepted.retain(|(at, _, _)| now.duration_since(*at) < std::time::Duration::from_secs(60));
        if accepted.len() >= 1000 {
            return RateVerdict::EngineLimited;
        }
        let lifecycle = matches!(trigger, TriggerKind::ToolBefore | TriggerKind::ToolAfter);
        if lifecycle
            && accepted
                .iter()
                .filter(|(_, id, kind)| id == hook_id && *kind == trigger)
                .count()
                >= 60
        {
            return RateVerdict::HookLimited;
        }
        accepted.push_back((now, hook_id.to_string(), trigger));
        RateVerdict::Accepted
    }
}

impl Default for Dispatcher {
    fn default() -> Self {
        Self::new()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Type)]
pub enum TriggerKind {
    #[serde(rename = "session.start")]
    SessionStart,
    #[serde(rename = "tool.before")]
    ToolBefore,
    #[serde(rename = "tool.after")]
    ToolAfter,
    #[serde(rename = "session.end")]
    SessionEnd,
    #[serde(rename = "scheduler.run.success")]
    SchedulerRunSuccess,
    #[serde(rename = "scheduler.run.failed")]
    SchedulerRunFailed,
    #[serde(rename = "gateway.status.changed")]
    GatewayStatusChanged,
    #[serde(rename = "webhook.inbound")]
    WebhookInbound,
}

impl TriggerKind {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::SessionStart => "session.start",
            Self::ToolBefore => "tool.before",
            Self::ToolAfter => "tool.after",
            Self::SessionEnd => "session.end",
            Self::SchedulerRunSuccess => "scheduler.run.success",
            Self::SchedulerRunFailed => "scheduler.run.failed",
            Self::GatewayStatusChanged => "gateway.status.changed",
            Self::WebhookInbound => "webhook.inbound",
        }
    }
}

impl FromStr for TriggerKind {
    type Err = anyhow::Error;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value {
            "session.start" => Ok(Self::SessionStart),
            "tool.before" => Ok(Self::ToolBefore),
            "tool.after" => Ok(Self::ToolAfter),
            "session.end" => Ok(Self::SessionEnd),
            "scheduler.run.success" => Ok(Self::SchedulerRunSuccess),
            "scheduler.run.failed" => Ok(Self::SchedulerRunFailed),
            "gateway.status.changed" => Ok(Self::GatewayStatusChanged),
            "webhook.inbound" => Ok(Self::WebhookInbound),
            _ => bail!("unknown automation trigger kind: {value}"),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Type)]
pub enum ActionKind {
    #[serde(rename = "agent.run")]
    AgentRun,
    #[serde(rename = "webhook.outbound")]
    WebhookOutbound,
}

impl ActionKind {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::AgentRun => "agent.run",
            Self::WebhookOutbound => "webhook.outbound",
        }
    }
}

impl FromStr for ActionKind {
    type Err = anyhow::Error;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value {
            "agent.run" => Ok(Self::AgentRun),
            "webhook.outbound" => Ok(Self::WebhookOutbound),
            _ => bail!("unknown automation action kind: {value}"),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Type)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AgentRunAction {
    pub project_id: String,
    pub branch: String,
    pub gateway_id: String,
    pub prompt: String,
    pub agent_id: Option<String>,
    pub model_override: Option<String>,
    pub subtask: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Type)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct WebhookHeader {
    pub name: String,
    pub value: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Type)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct WebhookOutboundAction {
    pub url: String,
    pub method: String,
    #[serde(default)]
    pub headers: Vec<WebhookHeader>,
    pub payload_template: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Type)]
#[serde(
    tag = "kind",
    content = "config",
    rename_all = "camelCase",
    deny_unknown_fields
)]
pub enum HookActionInput {
    #[serde(rename = "agent.run")]
    AgentRun(AgentRunAction),
    #[serde(rename = "webhook.outbound")]
    WebhookOutbound(WebhookOutboundAction),
}

impl HookActionInput {
    pub const fn kind(&self) -> ActionKind {
        match self {
            Self::AgentRun(_) => ActionKind::AgentRun,
            Self::WebhookOutbound(_) => ActionKind::WebhookOutbound,
        }
    }

    pub fn agent_run(&self) -> Option<&AgentRunAction> {
        match self {
            Self::AgentRun(config) => Some(config),
            Self::WebhookOutbound(_) => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Type)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct HookInput {
    pub name: String,
    pub trigger_kind: TriggerKind,
    pub action: HookActionInput,
    #[serde(default = "enabled_by_default")]
    pub enabled: bool,
}

const fn enabled_by_default() -> bool {
    true
}

impl HookInput {
    pub fn agent_run(
        name: impl Into<String>,
        trigger_kind: TriggerKind,
        project_id: impl Into<String>,
        branch: impl Into<String>,
        gateway_id: impl Into<String>,
        prompt: impl Into<String>,
    ) -> Self {
        Self {
            name: name.into(),
            trigger_kind,
            action: HookActionInput::AgentRun(AgentRunAction {
                project_id: project_id.into(),
                branch: branch.into(),
                gateway_id: gateway_id.into(),
                prompt: prompt.into(),
                agent_id: None,
                model_override: None,
                subtask: false,
            }),
            enabled: true,
        }
    }

    pub fn outbound(
        name: impl Into<String>,
        trigger_kind: TriggerKind,
        url: impl Into<String>,
        payload_template: Option<String>,
    ) -> Self {
        Self {
            name: name.into(),
            trigger_kind,
            action: HookActionInput::WebhookOutbound(WebhookOutboundAction {
                url: url.into(),
                method: "POST".to_string(),
                headers: Vec::new(),
                payload_template,
            }),
            enabled: true,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct HookRow {
    pub id: String,
    pub name: String,
    pub trigger_kind: TriggerKind,
    pub action_kind: ActionKind,
    pub enabled: bool,
    pub inbound_path: Option<String>,
    pub action: HookActionInput,
    pub created_at: i64,
    pub updated_at: i64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct HookRunRow {
    pub id: String,
    pub hook_id: String,
    pub status: String,
    pub envelope: Value,
    pub snapshot: HookRow,
    pub session_pk: Option<String>,
    pub error: Option<String>,
    pub attempt_count: i64,
    pub last_http_status: Option<i64>,
    pub queued_at: i64,
    pub started_at: Option<i64>,
    pub finished_at: Option<i64>,
    #[serde(default)]
    pub attempts: Vec<HookAttemptRow>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct HookAttemptRow {
    pub run_id: String,
    pub ordinal: i64,
    pub started_at: i64,
    pub finished_at: Option<i64>,
    pub http_status: Option<i64>,
    pub error: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct HookDetail {
    pub hook: HookRow,
    pub runs: Vec<HookRunRow>,
}

fn normalize_name(name: &str) -> anyhow::Result<String> {
    let trimmed = name.trim();
    let length = trimmed.chars().count();
    if !(1..=120).contains(&length) {
        bail!("hook name must contain 1 through 120 Unicode scalar values");
    }
    Ok(trimmed.to_string())
}

fn validate_outbound_url(value: &str) -> anyhow::Result<url::Url> {
    let url = url::Url::parse(value).context("outbound URL must be valid")?;
    if !matches!(url.scheme(), "http" | "https") {
        bail!("outbound URL scheme must be http or https");
    }
    if !url.username().is_empty() || url.password().is_some() {
        bail!("outbound URL must not include userinfo");
    }
    if let Some(port) = url.port() {
        if url.scheme() == "https" && port != 443 {
            bail!("outbound HTTPS URLs only permit port 443");
        }
    }
    // HTTP is restricted to loopback, but accepts explicit ports for local development and tests.
    let host = url.host_str().context("outbound URL must include a host")?;
    let ip_host = host.trim_matches(['[', ']']);
    match ip_host.parse::<IpAddr>() {
        Ok(ip) if url.scheme() == "http" && is_loopback_ip(ip) => {}
        Ok(_) => bail!("outbound URL IP literals are not permitted"),
        Err(_) if url.scheme() == "http" && host.eq_ignore_ascii_case("localhost") => {}
        Err(_) if url.scheme() == "https" => {}
        Err(_) => bail!("HTTP outbound URLs are only permitted for localhost"),
    }
    Ok(url)
}

fn is_loopback_ip(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(ip) => ip.is_loopback(),
        IpAddr::V6(ip) => ip.is_loopback(),
    }
}

fn is_public_outbound_ip(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(ip) => {
            !(ip.is_private()
                || ip.is_loopback()
                || ip.is_link_local()
                || ip.is_unspecified()
                || ip.is_multicast()
                || ip.octets()[0] == 100 && (64..=127).contains(&ip.octets()[1])
                || ip.octets()[0] >= 224
                || ip == Ipv4Addr::new(0, 0, 0, 0))
        }
        IpAddr::V6(ip) => {
            if let Some(mapped) = ip.to_ipv4_mapped() {
                return is_public_outbound_ip(IpAddr::V4(mapped));
            }
            !(ip.is_loopback()
                || ip.is_unspecified()
                || ip.is_multicast()
                || ip.is_unique_local()
                || ip.is_unicast_link_local())
        }
    }
}
fn validate_action(trigger_kind: TriggerKind, action: &HookActionInput) -> anyhow::Result<()> {
    if trigger_kind == TriggerKind::WebhookInbound && action.kind() != ActionKind::AgentRun {
        bail!("webhook.inbound hooks only support agent.run");
    }

    match action {
        HookActionInput::AgentRun(config) => {
            if config.project_id.trim().is_empty() {
                bail!("agent.run requires a project ID");
            }
            if config.gateway_id != "local" {
                bail!("agent.run hooks may only target the local gateway");
            }
            if config.branch.len() > 256 {
                bail!("branch must be at most 256 bytes");
            }
            let prompt_length = config.prompt.len();
            if !(1..=32 * 1024).contains(&prompt_length) {
                bail!("agent.run prompt must be between 1 and 32768 bytes");
            }
        }
        HookActionInput::WebhookOutbound(config) => {
            if config.url.len() > 2 * 1024 {
                bail!("outbound URL must be at most 2048 bytes");
            }
            let _ = validate_outbound_url(&config.url)?;
            if config.method != "POST" {
                bail!("outbound webhook method must be POST");
            }
            if config.headers.len() > 20 {
                bail!("outbound webhook supports at most 20 headers");
            }
            for header in &config.headers {
                if header.name.is_empty() || header.name.len() > 128 {
                    bail!("outbound header names must contain 1 through 128 bytes");
                }
                reqwest::header::HeaderName::from_bytes(header.name.as_bytes())
                    .context("outbound header name must be a valid HTTP header name")?;
                if header.value.len() > 4 * 1024 {
                    bail!("outbound header values must be at most 4096 bytes");
                }
            }
            if let Some(template) = config.payload_template.as_deref() {
                let representative_event = serde_json::json!({ "event": "automation.test" });
                let representative_run = serde_json::json!({
                    "id": "run-validation",
                    "hookId": "hook-validation",
                    "attempt": 1,
                    "test": false,
                });
                render_outbound_payload(
                    Some(template),
                    &representative_event,
                    &representative_run,
                )?;
            }
        }
    }
    Ok(())
}

fn validate_input(input: &HookInput) -> anyhow::Result<String> {
    let name = normalize_name(&input.name)?;
    validate_action(input.trigger_kind, &input.action)?;
    Ok(name)
}

fn inbound_path(trigger_kind: TriggerKind) -> Option<String> {
    (trigger_kind == TriggerKind::WebhookInbound).then(|| format!("wh_{}", Uuid::new_v4().simple()))
}

const HOOK_COLUMNS: &str =
    "id,name,trigger_kind,action_kind,enabled,inbound_path,config_json,created_at,updated_at";
const RUN_COLUMNS: &str = "id,hook_id,status,envelope_json,snapshot_json,session_pk,error,attempt_count,last_http_status,queued_at,started_at,finished_at";

fn sql_json_error(err: impl Into<Box<dyn std::error::Error + Send + Sync>>) -> rusqlite::Error {
    rusqlite::Error::ToSqlConversionFailure(err.into())
}

pub async fn deliver_outbound_once(
    action: &WebhookOutboundAction,
    payload: &Value,
) -> anyhow::Result<u16> {
    let url = validate_outbound_url(&action.url)?;
    let host = url.host_str().context("outbound URL must include a host")?;
    let port = url
        .port_or_known_default()
        .context("outbound URL must include a port")?;
    let addresses = tokio::net::lookup_host((host, port))
        .await
        .context("could not resolve outbound webhook host")?
        .collect::<Vec<_>>();
    if addresses.is_empty() {
        bail!("outbound webhook host did not resolve");
    }
    let local_http = url.scheme() == "http";
    if local_http {
        if addresses
            .iter()
            .any(|address| !is_loopback_ip(address.ip()))
        {
            bail!("localhost webhook resolved to a non-loopback address");
        }
    } else if addresses
        .iter()
        .any(|address| !is_public_outbound_ip(address.ip()))
    {
        bail!("outbound webhook host resolved to a non-public address");
    }

    let mut headers = reqwest::header::HeaderMap::new();
    headers.insert(
        reqwest::header::CONTENT_TYPE,
        reqwest::header::HeaderValue::from_static("application/json"),
    );
    for header in &action.headers {
        let name = reqwest::header::HeaderName::from_bytes(header.name.as_bytes())?;
        let value = reqwest::header::HeaderValue::from_str(&header.value)
            .context("outbound webhook header value is invalid")?;
        headers.insert(name, value);
    }
    let mut client = reqwest::Client::builder()
        .connect_timeout(std::time::Duration::from_secs(10))
        .timeout(std::time::Duration::from_secs(10))
        .no_proxy()
        .redirect(reqwest::redirect::Policy::none());
    for address in addresses {
        client = client.resolve(host, address);
    }
    let response = client
        .build()
        .context("could not build outbound webhook client")?
        .post(url)
        .headers(headers)
        .json(payload)
        .send()
        .await
        .context("outbound webhook request failed")?;
    Ok(response.status().as_u16())
}

pub async fn record_outbound_attempt(
    store: &Store,
    run_id: &str,
    ordinal: i64,
    result: Result<u16, &anyhow::Error>,
) -> anyhow::Result<bool> {
    let run_id = run_id.to_string();
    let started_at = now_ms();
    let (status, error) = match result {
        Ok(status) => (Some(i64::from(status)), None),
        Err(error) => (None, Some(sanitize_error(&error.to_string()))),
    };
    let succeeded = status.is_some_and(|status| (200..300).contains(&status));
    store
        .with_conn(move |c| {
            c.execute(
                "INSERT INTO automation_hook_attempts(run_id,ordinal,started_at,finished_at,http_status,error) VALUES (?1,?2,?3,?3,?4,?5)",
                params![run_id, ordinal, started_at, status, error],
            )?;
            c.execute(
                "UPDATE automation_hook_runs SET attempt_count=?2,last_http_status=?3,started_at=COALESCE(started_at,?4) WHERE id=?1",
                params![run_id, ordinal, status, started_at],
            )?;
            Ok(succeeded)
        })
        .await
}

pub async fn deliver_outbound(store: &Store, run: &HookRunRow) -> anyhow::Result<()> {
    deliver_outbound_with_retry(store, run, &OUTBOUND_RETRY_DELAYS, tokio::time::sleep).await
}

async fn deliver_outbound_with_retry<F, Fut>(
    store: &Store,
    run: &HookRunRow,
    retry_delays: &[std::time::Duration],
    mut sleep: F,
) -> anyhow::Result<()>
where
    F: FnMut(std::time::Duration) -> Fut,
    Fut: std::future::Future<Output = ()>,
{
    let HookActionInput::WebhookOutbound(action) = &run.snapshot.action else {
        bail!("automation run is not an outbound webhook");
    };
    for ordinal in 1..=retry_delays.len() as i64 {
        let run_value = serde_json::json!({
            "id": run.id,
            "hookId": run.hook_id,
            "attempt": ordinal,
            "test": false,
        });
        let payload = render_outbound_payload(
            action.payload_template.as_deref(),
            &run.envelope,
            &run_value,
        );
        let result = match payload {
            Ok(payload) => deliver_outbound_once(action, &payload).await,
            Err(error) => Err(error),
        };
        let succeeded =
            record_outbound_attempt(store, &run.id, ordinal, result.as_ref().copied()).await?;
        if succeeded {
            return Ok(());
        }
        if ordinal < retry_delays.len() as i64 {
            sleep(retry_delays[ordinal as usize - 1]).await;
        }
    }
    bail!("outbound webhook delivery failed")
}
fn encode_action_for_storage(action: &HookActionInput) -> anyhow::Result<String> {
    let mut action = action.clone();
    if let HookActionInput::WebhookOutbound(config) = &mut action {
        for header in &mut config.headers {
            header.value = secrets::encrypt_field(&header.value);
        }
    }
    serde_json::to_string(&action).context("could not serialize automation hook action")
}

fn decode_action_from_storage(config_json: &str) -> anyhow::Result<HookActionInput> {
    let mut action: HookActionInput = serde_json::from_str(config_json)
        .context("could not deserialize automation hook action")?;
    if let HookActionInput::WebhookOutbound(config) = &mut action {
        for header in &mut config.headers {
            if header.value.starts_with("enc:v1:") {
                header.value = secrets::decrypt_field(&header.value)
                    .context("could not decrypt outbound webhook header")?;
            }
        }
    }
    Ok(action)
}
fn hook_from(row: &Row<'_>) -> rusqlite::Result<HookRow> {
    let trigger: String = row.get(2)?;
    let action_kind: String = row.get(3)?;
    let config_json: String = row.get(6)?;
    Ok(HookRow {
        id: row.get(0)?,
        name: row.get(1)?,
        trigger_kind: TriggerKind::from_str(&trigger).map_err(sql_json_error)?,
        action_kind: ActionKind::from_str(&action_kind).map_err(sql_json_error)?,
        enabled: row.get::<_, i64>(4)? != 0,
        inbound_path: row.get(5)?,
        action: decode_action_from_storage(&config_json).map_err(sql_json_error)?,
        created_at: row.get(7)?,
        updated_at: row.get(8)?,
    })
}

fn snapshot_json_for_storage(snapshot: &HookRow) -> anyhow::Result<String> {
    let mut value = serde_json::to_value(snapshot).context("could not serialize hook snapshot")?;
    value["action"] = serde_json::from_str(&encode_action_for_storage(&snapshot.action)?)
        .context("could not serialize encrypted hook snapshot action")?;
    serde_json::to_string(&value).context("could not serialize encrypted hook snapshot")
}

fn snapshot_from_storage(snapshot_json: &str) -> anyhow::Result<HookRow> {
    let mut snapshot: HookRow =
        serde_json::from_str(snapshot_json).context("could not deserialize hook snapshot")?;
    snapshot.action = decode_action_from_storage(
        &serde_json::to_string(&snapshot.action)
            .context("could not serialize stored hook snapshot action")?,
    )?;
    Ok(snapshot)
}

fn run_from(row: &Row<'_>) -> rusqlite::Result<HookRunRow> {
    let envelope_json: String = row.get(3)?;
    let snapshot_json: String = row.get(4)?;
    Ok(HookRunRow {
        id: row.get(0)?,
        hook_id: row.get(1)?,
        status: row.get(2)?,
        envelope: serde_json::from_str(&envelope_json).map_err(sql_json_error)?,
        snapshot: snapshot_from_storage(&snapshot_json).map_err(sql_json_error)?,
        session_pk: row.get(5)?,
        error: row.get(6)?,
        attempt_count: row.get(7)?,
        last_http_status: row.get(8)?,
        queued_at: row.get(9)?,
        started_at: row.get(10)?,
        finished_at: row.get(11)?,
        attempts: Vec::new(),
    })
}

fn attempt_from(row: &Row<'_>) -> rusqlite::Result<HookAttemptRow> {
    Ok(HookAttemptRow {
        run_id: row.get(0)?,
        ordinal: row.get(1)?,
        started_at: row.get(2)?,
        finished_at: row.get(3)?,
        http_status: row.get(4)?,
        error: row.get(5)?,
    })
}

pub async fn create_hook(store: &Store, input: HookInput) -> anyhow::Result<HookRow> {
    let name = validate_input(&input)?;
    let now = now_ms();
    let hook = HookRow {
        id: new_id(),
        name,
        trigger_kind: input.trigger_kind,
        action_kind: input.action.kind(),
        enabled: input.enabled,
        inbound_path: inbound_path(input.trigger_kind),
        action: input.action,
        created_at: now,
        updated_at: now,
    };
    let stored = hook.clone();
    store
        .with_conn(move |c| {
            let config_json = encode_action_for_storage(&stored.action)
                .map_err(sql_json_error)?;
            c.execute(
                "INSERT INTO automation_hooks(id,name,trigger_kind,action_kind,enabled,inbound_path,config_json,created_at,updated_at) \
                 VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9)",
                params![
                    stored.id,
                    stored.name,
                    stored.trigger_kind.as_str(),
                    stored.action_kind.as_str(),
                    stored.enabled as i64,
                    stored.inbound_path,
                    config_json,
                    stored.created_at,
                    stored.updated_at,
                ],
            )?;
            Ok(())
        })
        .await?;
    Ok(hook)
}

pub async fn update_hook(store: &Store, id: &str, input: HookInput) -> anyhow::Result<HookRow> {
    let name = validate_input(&input)?;
    let id = id.to_string();
    let now = now_ms();
    store
        .with_conn(move |c| {
            let config_json = encode_action_for_storage(&input.action)
                .map_err(sql_json_error)?;
            let changed = c.execute(
                "UPDATE automation_hooks SET name=?2, trigger_kind=?3, action_kind=?4, enabled=?5, \
                 inbound_path=CASE WHEN ?3='webhook.inbound' THEN COALESCE(inbound_path, ?6) ELSE NULL END, \
                 config_json=?7, updated_at=?8 WHERE id=?1",
                params![
                    id,
                    name,
                    input.trigger_kind.as_str(),
                    input.action.kind().as_str(),
                    input.enabled as i64,
                    inbound_path(input.trigger_kind),
                    config_json,
                    now,
                ],
            )?;
            if changed == 0 {
                return Err(rusqlite::Error::QueryReturnedNoRows);
            }
            c.query_row(
                &format!("SELECT {HOOK_COLUMNS} FROM automation_hooks WHERE id=?1"),
                params![id],
                hook_from,
            )
        })
        .await
}

pub async fn list_hooks(store: &Store) -> anyhow::Result<Vec<HookRow>> {
    store
        .with_conn(|c| {
            let mut statement = c.prepare(&format!(
                "SELECT {HOOK_COLUMNS} FROM automation_hooks ORDER BY created_at DESC"
            ))?;
            let hooks = statement
                .query_map([], hook_from)?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            Ok(hooks)
        })
        .await
}

pub async fn find_inbound_hook(store: &Store, path: &str) -> anyhow::Result<Option<HookRow>> {
    let path = path.to_string();
    store
        .with_conn(move |c| {
            c.query_row(
                &format!(
                    "SELECT {HOOK_COLUMNS} FROM automation_hooks \
                     WHERE trigger_kind='webhook.inbound' AND inbound_path=?1"
                ),
                params![path],
                hook_from,
            )
            .optional()
        })
        .await
}

pub async fn list_enabled_hooks(
    store: &Store,
    trigger: TriggerKind,
) -> anyhow::Result<Vec<HookRow>> {
    let trigger = trigger.as_str().to_string();
    store
        .with_conn(move |c| {
            let mut statement = c.prepare(&format!(
                "SELECT {HOOK_COLUMNS} FROM automation_hooks WHERE enabled=1 AND trigger_kind=?1 ORDER BY created_at DESC"
            ))?;
            let hooks = statement
                .query_map(params![trigger], hook_from)?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            Ok(hooks)
        })
        .await
}

pub async fn finish_run(
    store: &Store,
    run_id: &str,
    status: &str,
    session_pk: Option<&str>,
    error: Option<&str>,
) -> anyhow::Result<bool> {
    let run_id = run_id.to_string();
    let status = status.to_string();
    let session_pk = session_pk.map(ToString::to_string);
    let error = error.map(|value| sanitize_error(value));
    let now = now_ms();
    store
        .with_conn(move |c| {
            c.execute(
                "UPDATE automation_hook_runs
                 SET status=?2, session_pk=COALESCE(?3, session_pk), error=?4,
                     started_at=COALESCE(started_at, ?5), finished_at=?5
                 WHERE id=?1 AND status IN ('queued', 'running')",
                params![run_id, status, session_pk, error, now],
            )
            .map(|changed| changed > 0)
        })
        .await
}

pub async fn mark_run_queued_not_dispatched(
    store: &Store,
    run_id: &str,
    error: &str,
) -> anyhow::Result<()> {
    let run_id = run_id.to_string();
    let error = sanitize_error(error);
    store
        .with_conn(move |c| {
            c.execute(
                "UPDATE automation_hook_runs SET status='queued', error=?2 WHERE id=?1",
                params![run_id, error],
            )?;
            Ok(())
        })
        .await
}

pub async fn link_run_session(
    store: &Store,
    run_id: &str,
    session_pk: &str,
) -> anyhow::Result<bool> {
    let run_id = run_id.to_string();
    let session_pk = session_pk.to_string();
    let now = now_ms();
    store
        .with_conn(move |c| {
            c.execute(
                "UPDATE automation_hook_runs
             SET status='running', session_pk=?2, started_at=?3
             WHERE id=?1 AND status='queued'",
                params![run_id, session_pk, now],
            )
            .map(|changed| changed > 0)
        })
        .await
}

fn sanitize_error(error: &str) -> String {
    error.chars().take(1024).collect()
}

pub async fn delete_hook(store: &Store, id: &str) -> anyhow::Result<()> {
    let id = id.to_string();
    store
        .with_conn(move |c| {
            c.execute("DELETE FROM automation_hooks WHERE id=?1", params![id])?;
            Ok(())
        })
        .await
}

pub async fn toggle_hook(store: &Store, id: &str, enabled: bool) -> anyhow::Result<()> {
    let id = id.to_string();
    let updated_at = now_ms();
    store
        .with_conn(move |c| {
            let changed = c.execute(
                "UPDATE automation_hooks SET enabled=?2, updated_at=?3 WHERE id=?1",
                params![id, enabled as i64, updated_at],
            )?;
            if changed == 0 {
                return Err(rusqlite::Error::QueryReturnedNoRows);
            }
            Ok(())
        })
        .await
}

pub async fn create_run(
    store: &Store,
    hook_id: &str,
    envelope: Value,
) -> anyhow::Result<HookRunRow> {
    create_run_at(store, hook_id, envelope, now_ms()).await
}

async fn create_run_at(
    store: &Store,
    hook_id: &str,
    envelope: Value,
    queued_at: i64,
) -> anyhow::Result<HookRunRow> {
    let hook_id = hook_id.to_string();
    let hook = store
        .with_conn(move |c| {
            c.query_row(
                &format!("SELECT {HOOK_COLUMNS} FROM automation_hooks WHERE id=?1"),
                params![hook_id],
                hook_from,
            )
            .optional()
        })
        .await?
        .context("automation hook does not exist")?;
    let run = HookRunRow {
        id: new_id(),
        hook_id: hook.id.clone(),
        status: "queued".to_string(),
        envelope,
        snapshot: hook,
        session_pk: None,
        error: None,
        attempt_count: 0,
        last_http_status: None,
        queued_at,
        started_at: None,
        finished_at: None,
        attempts: Vec::new(),
    };
    let stored = run.clone();
    store
        .with_conn(move |c| {
            let envelope_json = serde_json::to_string(&stored.envelope)
                .map_err(sql_json_error)?;
            let snapshot_json = snapshot_json_for_storage(&stored.snapshot)
                .map_err(sql_json_error)?;
            c.execute(
                "INSERT INTO automation_hook_runs(id,hook_id,status,envelope_json,snapshot_json,session_pk,error,attempt_count,last_http_status,queued_at,started_at,finished_at) \
                 VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12)",
                params![
                    stored.id,
                    stored.hook_id,
                    stored.status,
                    envelope_json,
                    snapshot_json,
                    stored.session_pk,
                    stored.error,
                    stored.attempt_count,
                    stored.last_http_status,
                    stored.queued_at,
                    stored.started_at,
                    stored.finished_at,
                ],
            )?;
            Ok(())
        })
        .await?;
    Ok(run)
}

pub async fn list_runs(store: &Store, hook_id: &str) -> anyhow::Result<Vec<HookRunRow>> {
    let hook_id = hook_id.to_string();
    store
        .with_conn(move |c| {
            let mut statement = c.prepare(&format!(
                "SELECT {RUN_COLUMNS} FROM automation_hook_runs \
                 WHERE hook_id=?1 ORDER BY queued_at DESC LIMIT ?2"
            ))?;
            let runs = statement
                .query_map(params![hook_id, MAX_RUNS], run_from)?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            Ok(runs)
        })
        .await
}

pub async fn hook_detail(store: &Store, id: &str) -> anyhow::Result<Option<HookDetail>> {
    let id = id.to_string();
    store
        .with_conn(move |c| {
            let hook = c
                .query_row(
                    &format!("SELECT {HOOK_COLUMNS} FROM automation_hooks WHERE id=?1"),
                    params![id],
                    hook_from,
                )
                .optional()?;
            let Some(hook) = hook else {
                return Ok(None);
            };
            let mut statement = c.prepare(&format!(
                "SELECT {RUN_COLUMNS} FROM automation_hook_runs \
                 WHERE hook_id=?1 ORDER BY queued_at DESC LIMIT ?2"
            ))?;
            let mut runs = statement
                .query_map(params![hook.id, MAX_RUNS], run_from)?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            let mut attempts = c.prepare(
                "SELECT run_id,ordinal,started_at,finished_at,http_status,error \
                 FROM automation_hook_attempts WHERE run_id=?1 \
                 ORDER BY ordinal DESC LIMIT ?2",
            )?;
            for run in &mut runs {
                run.attempts = attempts
                    .query_map(params![run.id, MAX_DETAIL_ATTEMPTS], attempt_from)?
                    .collect::<rusqlite::Result<Vec<_>>>()?;
                run.attempts.reverse();
            }
            Ok(Some(HookDetail { hook, runs }))
        })
        .await
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{
        http::StatusCode,
        response::Redirect,
        routing::{get, post},
        Router,
    };
    use serde_json::json;
    use std::sync::{
        atomic::{AtomicUsize, Ordering},
        Arc,
    };
    use tokio::sync::oneshot;

    async fn loopback_status_server(
        statuses: Vec<StatusCode>,
    ) -> (String, Arc<AtomicUsize>, oneshot::Sender<()>) {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let statuses = Arc::new(statuses);
        let requests = Arc::new(AtomicUsize::new(0));
        let handler_statuses = Arc::clone(&statuses);
        let handler_requests = Arc::clone(&requests);
        let app = Router::new().route(
            "/",
            post(move || {
                let statuses = Arc::clone(&handler_statuses);
                let ordinal = handler_requests.fetch_add(1, Ordering::SeqCst);
                async move { statuses[ordinal.min(statuses.len() - 1)] }
            }),
        );
        let (shutdown, receiver) = oneshot::channel();
        tokio::spawn(async move {
            axum::serve(listener, app)
                .with_graceful_shutdown(async { _ = receiver.await })
                .await
                .unwrap();
        });
        (format!("http://{address}/"), requests, shutdown)
    }

    async fn loopback_redirect_server() -> (String, Arc<AtomicUsize>, oneshot::Sender<()>) {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let redirected = Arc::new(AtomicUsize::new(0));
        let redirected_handler = Arc::clone(&redirected);
        let app = Router::new()
            .route(
                "/",
                get(|| async { Redirect::temporary("/redirected") })
                    .post(|| async { Redirect::temporary("/redirected") }),
            )
            .route(
                "/redirected",
                post(move || {
                    let redirected = Arc::clone(&redirected_handler);
                    async move {
                        redirected.fetch_add(1, Ordering::SeqCst);
                        StatusCode::NO_CONTENT
                    }
                }),
            );
        let (shutdown, receiver) = oneshot::channel();
        tokio::spawn(async move {
            axum::serve(listener, app)
                .with_graceful_shutdown(async { _ = receiver.await })
                .await
                .unwrap();
        });
        (format!("http://{address}/"), redirected, shutdown)
    }

    #[test]
    fn envelope_caps_values_and_truncates_object_keys_lexically() {
        let envelope = AutomationEnvelope::new(
            TriggerKind::ToolAfter,
            "2026-07-10T00:00:00Z",
            AutomationSource::new("session", "s-1"),
            json!({
                "input": {
                    "z": "z".repeat(80 * 1024),
                    "a": "a".repeat(80 * 1024),
                },
            }),
        )
        .capped();

        let serialized = serde_json::to_vec(&envelope).unwrap();
        assert!(serialized.len() <= MAX_ENVELOPE_BYTES);
        assert!(envelope.data["input"]["a"]
            .as_str()
            .unwrap()
            .ends_with("…[truncated]"));
        assert!(envelope.data["input"].get("z").is_none());
    }

    #[test]
    fn envelope_caps_metadata_and_data_under_the_hard_limit() {
        let envelope = AutomationEnvelope::new(
            TriggerKind::ToolAfter,
            "t".repeat(MAX_ENVELOPE_BYTES),
            AutomationSource::new(
                "k".repeat(MAX_ENVELOPE_BYTES),
                "i".repeat(MAX_ENVELOPE_BYTES),
            ),
            json!({ "z": "z".repeat(MAX_ENVELOPE_BYTES) }),
        )
        .capped();

        let serialized = serde_json::to_vec(&envelope).unwrap();
        assert!(serialized.len() <= MAX_ENVELOPE_BYTES);
        assert!(envelope.source.kind.ends_with(TRUNCATED_SUFFIX));
        assert!(envelope.source.id.ends_with(TRUNCATED_SUFFIX));
        assert!(envelope.occurred_at.ends_with(TRUNCATED_SUFFIX));
    }

    #[test]
    fn envelope_hard_cap_includes_json_escaped_metadata() {
        let envelope = AutomationEnvelope::new(
            TriggerKind::ToolAfter,
            "\0".repeat(MAX_ENVELOPE_BYTES),
            AutomationSource::new(
                "\0".repeat(MAX_ENVELOPE_BYTES),
                "\0".repeat(MAX_ENVELOPE_BYTES),
            ),
            json!({ "z": "\0".repeat(MAX_ENVELOPE_BYTES) }),
        )
        .capped();

        assert!(serde_json::to_vec(&envelope).unwrap().len() <= MAX_ENVELOPE_BYTES);
    }

    #[tokio::test]
    async fn redirect_responses_are_not_followed() {
        let (store, _db) = mem_store().await;
        let (url, redirected, shutdown) = loopback_redirect_server().await;
        let hook = create_hook(
            &store,
            HookInput::outbound("No redirect", TriggerKind::SessionEnd, url, None),
        )
        .await
        .unwrap();
        let run = create_run(&store, &hook.id, json!({ "event": "session.end" }))
            .await
            .unwrap();

        let error =
            deliver_outbound_with_retry(&store, &run, &[std::time::Duration::ZERO], |_| async {})
                .await
                .unwrap_err();
        assert_eq!(error.to_string(), "outbound webhook delivery failed");
        assert_eq!(redirected.load(Ordering::SeqCst), 0);
        let _ = shutdown.send(());
    }

    #[tokio::test]
    async fn records_retry_history_without_waiting_and_marks_delivery_terminal() {
        let (store, _db) = mem_store().await;
        // Explicit loopback ports are intentionally allowed for local development and tests.
        let (url, requests, shutdown) = loopback_status_server(vec![
            StatusCode::INTERNAL_SERVER_ERROR,
            StatusCode::INTERNAL_SERVER_ERROR,
            StatusCode::INTERNAL_SERVER_ERROR,
        ])
        .await;
        let hook = create_hook(
            &store,
            HookInput::outbound("Retries", TriggerKind::SessionEnd, url, None),
        )
        .await
        .unwrap();
        let run = create_run(&store, &hook.id, json!({ "event": "session.end" }))
            .await
            .unwrap();

        let error = deliver_outbound_with_retry(
            &store,
            &run,
            &[
                std::time::Duration::ZERO,
                std::time::Duration::ZERO,
                std::time::Duration::ZERO,
            ],
            |_| async {},
        )
        .await
        .unwrap_err();
        assert_eq!(error.to_string(), "outbound webhook delivery failed");
        assert_eq!(requests.load(Ordering::SeqCst), 3);
        let stored = list_runs(&store, &hook.id).await.unwrap().pop().unwrap();
        assert_eq!(stored.attempt_count, 3);
        assert_eq!(stored.last_http_status, Some(500));
        let detail = hook_detail(&store, &hook.id).await.unwrap().unwrap();
        assert_eq!(
            detail.runs[0]
                .attempts
                .iter()
                .map(|attempt| (
                    attempt.ordinal,
                    attempt.http_status,
                    attempt.error.as_deref()
                ))
                .collect::<Vec<_>>(),
            vec![
                (1, Some(500), None),
                (2, Some(500), None),
                (3, Some(500), None)
            ]
        );
        let _ = shutdown.send(());
    }

    #[tokio::test]
    async fn records_successful_outbound_delivery() {
        let (store, _db) = mem_store().await;
        let (url, requests, shutdown) = loopback_status_server(vec![StatusCode::NO_CONTENT]).await;
        let hook = create_hook(
            &store,
            HookInput::outbound("Success", TriggerKind::SessionEnd, url, None),
        )
        .await
        .unwrap();
        let run = create_run(&store, &hook.id, json!({ "event": "session.end" }))
            .await
            .unwrap();

        deliver_outbound_with_retry(&store, &run, &OUTBOUND_RETRY_DELAYS, |_| async {})
            .await
            .unwrap();
        assert_eq!(requests.load(Ordering::SeqCst), 1);
        let stored = list_runs(&store, &hook.id).await.unwrap().pop().unwrap();
        assert_eq!(stored.attempt_count, 1);
        assert_eq!(stored.last_http_status, Some(204));
        let _ = shutdown.send(());
    }

    #[test]
    fn renders_default_and_placeholder_only_outbound_payloads() {
        let event = json!({ "kind": "session.end" });
        let run = json!({ "id": "run-1", "attempt": 2, "test": false });

        assert_eq!(render_outbound_payload(None, &event, &run).unwrap(), event);
        assert_eq!(
            render_outbound_payload(
                Some(r#"{"payload":"${event}","delivery":"${run}"}"#),
                &event,
                &run,
            )
            .unwrap(),
            json!({
                "payload": { "kind": "session.end" },
                "delivery": { "id": "run-1", "attempt": 2, "test": false }
            })
        );

        for template in [
            "not json",
            r#"{"event":"before ${event}"}"#,
            r#"{"event":"${unknown}"}"#,
        ] {
            assert!(render_outbound_payload(Some(template), &event, &run).is_err());
        }
    }

    #[tokio::test]
    async fn create_and_update_reject_invalid_outbound_payload_templates() {
        let (store, _db) = mem_store().await;
        for template in [
            "not json",
            r#"{"event":"prefix ${event}"}"#,
            r#"{"event":"${unknown}"}"#,
        ] {
            assert!(
                create_hook(
                    &store,
                    HookInput::outbound(
                        "Invalid template",
                        TriggerKind::SessionEnd,
                        "https://example.com/hook",
                        Some(template.into()),
                    ),
                )
                .await
                .is_err(),
                "{template:?} must be rejected"
            );
        }

        let hook = create_hook(
            &store,
            HookInput::outbound(
                "Valid template",
                TriggerKind::SessionEnd,
                "https://example.com/hook",
                Some(r#"{"payload":"${event}","delivery":"${run}"}"#.into()),
            ),
        )
        .await
        .unwrap();
        assert!(update_hook(
            &store,
            &hook.id,
            HookInput::outbound(
                "Valid template",
                TriggerKind::SessionEnd,
                "https://example.com/hook",
                Some(r#"{"payload":"prefix ${event}"}"#.into()),
            ),
        )
        .await
        .is_err());
    }

    #[test]
    fn outbound_url_policy_only_permits_safe_schemes_and_hosts() {
        for url in [
            "ftp://example.com/hook",
            "https://user@example.com/hook",
            "https://example.com:444/hook",
            "http://example.com/hook",
            "https://127.0.0.1/hook",
            "http://[::2]/hook",
        ] {
            assert!(
                validate_outbound_url(url).is_err(),
                "{url} must be rejected"
            );
        }
        for url in [
            "https://example.com/hook",
            "http://localhost/hook",
            "http://localhost:43123/hook", // Explicit loopback ports remain intentional.
            "http://127.0.0.1/hook",
            "http://[::1]/hook",
        ] {
            assert!(validate_outbound_url(url).is_ok(), "{url} must be accepted");
        }
    }

    #[test]
    fn rejects_private_nonpublic_and_ipv4_mapped_ipv6_outbound_addresses() {
        for address in [
            "10.0.0.1",
            "172.16.0.1",
            "192.168.0.1",
            "127.0.0.1",
            "169.254.0.1",
            "0.0.0.0",
            "100.64.0.1",
            "224.0.0.1",
            "::1",
            "::",
            "fc00::1",
            "fe80::1",
            "::ffff:127.0.0.1",
            "::ffff:10.0.0.1",
            "::ffff:192.168.0.1",
        ] {
            assert!(
                !is_public_outbound_ip(address.parse().unwrap()),
                "{address} must be rejected"
            );
        }
        assert!(is_public_outbound_ip("1.1.1.1".parse().unwrap()));
        assert!(is_public_outbound_ip(
            "2606:4700:4700::1111".parse().unwrap()
        ));
    }

    #[tokio::test]
    async fn outbound_headers_are_encrypted_at_rest_and_legacy_rows_read() {
        crate::llm_router::secrets::use_test_key_file();
        let (store, _db) = mem_store().await;
        let mut input = HookInput::outbound(
            "Encrypted headers",
            TriggerKind::SessionEnd,
            "https://example.com/hook",
            None,
        );
        let HookActionInput::WebhookOutbound(action) = &mut input.action else {
            unreachable!();
        };
        action.headers.push(WebhookHeader {
            name: "Authorization".into(),
            value: "Bearer secret-value".into(),
        });
        let hook = create_hook(&store, input).await.unwrap();

        let hook_id = hook.id.clone();
        let stored: String = store
            .with_conn(move |c| {
                c.query_row(
                    "SELECT config_json FROM automation_hooks WHERE id=?1",
                    params![hook_id],
                    |r| r.get(0),
                )
            })
            .await
            .unwrap();
        assert!(stored.contains("enc:v1:"));
        assert!(!stored.contains("Bearer secret-value"));
        let listed = list_hooks(&store).await.unwrap();
        let HookActionInput::WebhookOutbound(action) = &listed[0].action else {
            unreachable!();
        };
        assert_eq!(action.headers[0].value, "Bearer secret-value");

        let run = create_run(&store, &hook.id, json!({ "event": "session.end" }))
            .await
            .unwrap();
        let run_id = run.id.clone();
        let snapshot: String = store
            .with_conn(move |c| {
                c.query_row(
                    "SELECT snapshot_json FROM automation_hook_runs WHERE id=?1",
                    params![run_id],
                    |r| r.get(0),
                )
            })
            .await
            .unwrap();
        assert!(snapshot.contains("enc:v1:"));
        assert!(!snapshot.contains("Bearer secret-value"));

        let mut literal_input = HookInput::outbound(
            "Encrypted prefix",
            TriggerKind::SessionStart,
            "https://example.com/literal",
            None,
        );
        let HookActionInput::WebhookOutbound(action) = &mut literal_input.action else {
            unreachable!();
        };
        action.headers.push(WebhookHeader {
            name: "X-Literal".into(),
            value: "enc:ordinary-value".into(),
        });
        let literal = create_hook(&store, literal_input).await.unwrap();
        let literal_id = literal.id.clone();
        let stored_literal: String = store
            .with_conn(move |c| {
                c.query_row(
                    "SELECT config_json FROM automation_hooks WHERE id=?1",
                    params![literal_id],
                    |r| r.get(0),
                )
            })
            .await
            .unwrap();
        assert!(!stored_literal.contains("enc:ordinary-value"));
        let HookActionInput::WebhookOutbound(action) = &list_hooks(&store)
            .await
            .unwrap()
            .into_iter()
            .find(|hook| hook.id == literal.id)
            .unwrap()
            .action
        else {
            unreachable!();
        };
        assert_eq!(action.headers[0].value, "enc:ordinary-value");

        let legacy_id = hook.id.clone();
        store
            .with_conn(move |c| {
                c.execute(
                    "UPDATE automation_hooks SET config_json=?2 WHERE id=?1",
                    params![
                        hook.id,
                        r#"{"kind":"webhook.outbound","config":{"url":"https://example.com/hook","method":"POST","headers":[{"name":"X-Legacy","value":"legacy"}],"payloadTemplate":null}}"#
                    ],
                )?;
                Ok(())
            })
            .await
            .unwrap();
        let listed = list_hooks(&store).await.unwrap();
        let HookActionInput::WebhookOutbound(action) = &listed
            .into_iter()
            .find(|row| row.id == legacy_id)
            .unwrap()
            .action
        else {
            unreachable!();
        };
        assert_eq!(action.headers[0].value, "legacy");
    }

    #[tokio::test]
    async fn cancellation_terminal_status_survives_end_link_and_late_finish_race() {
        let (store, _db) = mem_store().await;
        let hook = create_hook(
            &store,
            HookInput::agent_run(
                "Monotonic",
                TriggerKind::SessionEnd,
                "project-1",
                "main",
                "local",
                "run",
            ),
        )
        .await
        .unwrap();
        let run = create_run(&store, &hook.id, json!({})).await.unwrap();

        // Link is the normal start path; stop/end finalizes it as failed. The
        // delayed startup link and successful turn completion must both lose.
        assert!(link_run_session(&store, &run.id, "session-1")
            .await
            .unwrap());
        assert!(finish_run(
            &store,
            &run.id,
            "failed",
            Some("session-1"),
            Some("session cancelled"),
        )
        .await
        .unwrap());
        assert!(!link_run_session(&store, &run.id, "session-2")
            .await
            .unwrap());
        assert!(
            !finish_run(&store, &run.id, "success", Some("session-2"), None)
                .await
                .unwrap()
        );

        let stored = list_runs(&store, &hook.id).await.unwrap();
        assert_eq!(stored[0].status, "failed");
        assert_eq!(stored[0].error.as_deref(), Some("session cancelled"));
        assert_eq!(stored[0].session_pk.as_deref(), Some("session-1"));
    }
    #[test]
    fn rate_verdict_skips_the_sixty_first_tool_lifecycle_execution() {
        let dispatcher = Dispatcher::new();
        for _ in 0..60 {
            assert_eq!(
                dispatcher.rate_verdict("hook-1", TriggerKind::ToolBefore, 0),
                RateVerdict::Accepted
            );
        }
        assert_eq!(
            dispatcher.rate_verdict("hook-1", TriggerKind::ToolBefore, 0),
            RateVerdict::HookLimited
        );
    }

    #[test]
    fn hook_origin_depth_three_is_not_dispatchable() {
        assert!(HookOrigin::new("hook-1", "run-1", 2).allows_dispatch());
        assert!(!HookOrigin::new("hook-1", "run-1", 3).allows_dispatch());
    }

    async fn mem_store() -> (Store, tempfile::NamedTempFile) {
        let db = tempfile::NamedTempFile::new().unwrap();
        let store = Store::open(db.path()).await.unwrap();
        (store, db)
    }

    #[tokio::test]
    async fn queued_not_dispatched_keeps_queued_status_and_sanitizes_error() {
        let (store, _db) = mem_store().await;
        let hook = create_hook(
            &store,
            HookInput::outbound(
                "Queued outbound",
                TriggerKind::SessionEnd,
                "https://example.com/hook",
                None,
            ),
        )
        .await
        .unwrap();
        let run = create_run(&store, &hook.id, json!({ "event": "session.end" }))
            .await
            .unwrap();

        mark_run_queued_not_dispatched(&store, &run.id, &format!("{}\nignored", "x".repeat(2048)))
            .await
            .unwrap();

        let stored = list_runs(&store, &hook.id).await.unwrap();
        assert_eq!(stored[0].status, "queued");
        assert!(stored[0].finished_at.is_none());
        assert_eq!(stored[0].error.as_deref().unwrap().chars().count(), 1024);
    }

    #[tokio::test]
    async fn creates_local_agent_hook_and_returns_its_immutable_config() {
        let (store, _db) = mem_store().await;
        let hook = create_hook(
            &store,
            HookInput::agent_run(
                "Notify",
                TriggerKind::SessionEnd,
                "project-1",
                "",
                "local",
                "Summarize $EVENT",
            ),
        )
        .await
        .unwrap();

        assert_eq!(hook.trigger_kind, TriggerKind::SessionEnd);
        assert_eq!(hook.action_kind, ActionKind::AgentRun);
        assert!(hook.inbound_path.is_none());
        assert_eq!(hook.name, "Notify");
    }

    #[tokio::test]
    async fn rejects_non_local_agent_target_and_inbound_outbound_pair() {
        let (store, _db) = mem_store().await;
        assert!(create_hook(
            &store,
            HookInput::agent_run(
                "remote",
                TriggerKind::SessionStart,
                "project-1",
                "",
                "ssh-1",
                "run",
            ),
        )
        .await
        .is_err());
        assert!(create_hook(
            &store,
            HookInput::outbound(
                "inbound outbound",
                TriggerKind::WebhookInbound,
                "https://example.com",
                None,
            ),
        )
        .await
        .is_err());
    }

    #[tokio::test]
    async fn rejects_non_http_urls_and_invalid_header_names() {
        let (store, _db) = mem_store().await;

        for url in ["ftp://example.com/hook", "file:///tmp/hook"] {
            assert!(
                create_hook(
                    &store,
                    HookInput::outbound("invalid URL", TriggerKind::SessionEnd, url, None),
                )
                .await
                .is_err(),
                "{url} must be rejected"
            );
        }

        for name in ["Bad Header", "Bad\nHeader", "Bad\u{7f}Header"] {
            let mut input = HookInput::outbound(
                "invalid header",
                TriggerKind::SessionEnd,
                "https://example.com/hook",
                None,
            );
            let HookActionInput::WebhookOutbound(config) = &mut input.action else {
                unreachable!();
            };
            config.headers.push(WebhookHeader {
                name: name.to_string(),
                value: "value".to_string(),
            });

            assert!(
                create_hook(&store, input).await.is_err(),
                "{name:?} must be rejected"
            );
        }
    }

    #[tokio::test]
    async fn inbound_agent_hook_generates_an_engine_owned_path() {
        let (store, _db) = mem_store().await;
        let hook = create_hook(
            &store,
            HookInput::agent_run(
                "Inbound",
                TriggerKind::WebhookInbound,
                "project-1",
                "",
                "local",
                "handle inbound",
            ),
        )
        .await
        .unwrap();

        let path = hook.inbound_path.unwrap();
        assert!(path.starts_with("wh_"));
        assert_eq!(path.len(), 35);
        assert!(path[3..]
            .chars()
            .all(|character| character.is_ascii_hexdigit()));
    }

    #[tokio::test]
    async fn run_snapshot_survives_hook_update_and_deletion() {
        let (store, _db) = mem_store().await;
        let hook = create_hook(
            &store,
            HookInput::agent_run(
                "Snapshot",
                TriggerKind::SessionEnd,
                "project-1",
                "main",
                "local",
                "first prompt",
            ),
        )
        .await
        .unwrap();
        let run = create_run(&store, &hook.id, json!({ "event": "session.end" }))
            .await
            .unwrap();

        update_hook(
            &store,
            &hook.id,
            HookInput::agent_run(
                "Snapshot updated",
                TriggerKind::SessionEnd,
                "project-1",
                "main",
                "local",
                "second prompt",
            ),
        )
        .await
        .unwrap();
        delete_hook(&store, &hook.id).await.unwrap();

        let stored = list_runs(&store, &hook.id).await.unwrap();
        assert_eq!(stored.len(), 1);
        assert_eq!(stored[0].id, run.id);
        assert_eq!(stored[0].snapshot.name, "Snapshot");
        assert_eq!(
            stored[0].snapshot.action.agent_run().unwrap().prompt,
            "first prompt"
        );
    }

    #[tokio::test]
    async fn hook_detail_limits_runs_and_attempts() {
        let (store, _db) = mem_store().await;
        let hook = create_hook(
            &store,
            HookInput::outbound(
                "Delivery",
                TriggerKind::SessionEnd,
                "https://example.com",
                None,
            ),
        )
        .await
        .unwrap();
        for queued_at in 0..21 {
            create_run_at(&store, &hook.id, json!({}), queued_at)
                .await
                .unwrap();
        }
        let newest = list_runs(&store, &hook.id).await.unwrap()[0].clone();
        store
            .with_conn(move |c| {
                for ordinal in 1..=4 {
                    c.execute(
                        "INSERT INTO automation_hook_attempts(run_id, ordinal, started_at) VALUES (?1, ?2, ?3)",
                        params![newest.id, ordinal, ordinal],
                    )?;
                }
                Ok(())
            })
            .await
            .unwrap();

        let detail = hook_detail(&store, &hook.id).await.unwrap().unwrap();
        assert_eq!(detail.runs.len(), 20);
        assert_eq!(detail.runs[0].attempts.len(), 3);
        assert_eq!(
            detail.runs[0]
                .attempts
                .iter()
                .map(|attempt| attempt.ordinal)
                .collect::<Vec<_>>(),
            vec![2, 3, 4]
        );
    }
}
