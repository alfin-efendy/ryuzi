//! Persisted automation Hook configuration and dispatch history.

use crate::paths::{new_id, now_ms};
use crate::store::Store;
use anyhow::{bail, Context};
use rusqlite::{params, OptionalExtension, Row};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use specta::Type;
use std::str::FromStr;
use uuid::Uuid;

const MAX_RUNS: u32 = 20;
const MAX_DETAIL_ATTEMPTS: u32 = 3;

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

    async fn mem_store() -> (Store, tempfile::NamedTempFile) {
        let db = tempfile::NamedTempFile::new().unwrap();
        let store = Store::open(db.path()).await.unwrap();
        (store, db)
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
