//! Persisted automation Hook configuration and dispatch history.

use crate::paths::{new_id, now_ms};
use crate::store::Store;
use anyhow::{bail, Context};
use futures::FutureExt;
use rusqlite::{params, OptionalExtension, Row};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use specta::Type;
use std::collections::VecDeque;
use std::str::FromStr;
use std::sync::{Arc, OnceLock};
use uuid::Uuid;

const MAX_RUNS: u32 = 20;
const MAX_DETAIL_ATTEMPTS: u32 = 3;
const MAX_TOOL_VALUE_BYTES: usize = 64 * 1024;
pub const MAX_ENVELOPE_BYTES: usize = 256 * 1024;
const TRUNCATED_SUFFIX: &str = "…[truncated]";

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
            let url = url::Url::parse(&config.url).context("outbound URL must be valid")?;
            if !matches!(url.scheme(), "http" | "https") {
                bail!("outbound URL scheme must be http or https");
            }
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
            if config
                .payload_template
                .as_deref()
                .is_some_and(|template| template.len() > 64 * 1024)
            {
                bail!("outbound payload template must be at most 65536 bytes");
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
        action: serde_json::from_str(&config_json).map_err(sql_json_error)?,
        created_at: row.get(7)?,
        updated_at: row.get(8)?,
    })
}

fn run_from(row: &Row<'_>) -> rusqlite::Result<HookRunRow> {
    let envelope_json: String = row.get(3)?;
    let snapshot_json: String = row.get(4)?;
    Ok(HookRunRow {
        id: row.get(0)?,
        hook_id: row.get(1)?,
        status: row.get(2)?,
        envelope: serde_json::from_str(&envelope_json).map_err(sql_json_error)?,
        snapshot: serde_json::from_str(&snapshot_json).map_err(sql_json_error)?,
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
            let config_json = serde_json::to_string(&stored.action)
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
            let config_json = serde_json::to_string(&input.action)
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
            let snapshot_json = serde_json::to_string(&stored.snapshot)
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
    use serde_json::json;

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
