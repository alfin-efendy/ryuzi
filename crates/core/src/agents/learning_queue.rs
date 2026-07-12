//! Durable, strictly ordered per-agent learning queue.
//!
//! Every learning event is one row in `agent_learning_queue` with a
//! per-agent monotonic sequence allocated from `agent_learning_state`
//! inside a single Immediate transaction. Workers drain one agent at a
//! time in strict sequence order: `claim_next` only ever hands out the
//! lowest non-delivered sequence, so a stuck head-of-line event blocks
//! that agent (until its claim goes stale and is reclaimed) without
//! blocking any other agent. Application into the OKF bundle is
//! idempotent — every produced concept records the event id in
//! frontmatter, and a replay that finds the id already recorded is a
//! no-op — so the crash window between apply and acknowledge cannot
//! duplicate knowledge.

use std::sync::Arc;

use anyhow::{anyhow, bail, Context};
use chrono::{DateTime, Utc};
use indexmap::IndexMap;
use rusqlite::{params, OptionalExtension, TransactionBehavior};
use serde::{Deserialize, Serialize};
use serde_yaml::Value;

use crate::harness::native::memory::{validate_budget, MemoryOperation, MemoryScope};
use crate::paths;
use crate::store::Store;

use super::knowledge::{AgentKnowledgeStore, KnowledgeScan, LearningEventWrites};
use super::okf::{
    render_concept, validate_path_component, ConceptArea, KnowledgeConcept, KnowledgeScope,
    RESERVED_FILE_NAMES,
};
use super::registry::validate_agent_id;

/// How long a claim may sit unacknowledged before another worker may
/// reclaim it. Long enough for a slow apply, short enough that a crashed
/// worker does not park an agent's learning for long.
const DEFAULT_STALE_AFTER_MS: i64 = 5 * 60 * 1000;

/// `last_error` cap, in characters — errors are diagnostics, not storage.
const MAX_LAST_ERROR_CHARS: usize = 500;

/// One durable learning event as stored in the queue.
#[derive(Debug, Clone, PartialEq)]
pub struct LearningEvent {
    pub event_id: String,
    pub agent_id: String,
    pub sequence: i64,
    pub payload: LearningEventPayload,
    pub attempts: u32,
}

/// A memory mutation the agent asked to persist. `project_id` scopes
/// project-memory operations; global/user operations leave it `None`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MemoryLearningEvent {
    pub operation: MemoryOperation,
    pub source: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub project_id: Option<String>,
}

/// One observed skill invocation to fold into that skill's usage counters.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SkillUsageEvent {
    pub skill_id: String,
    pub succeeded: bool,
    pub source: String,
}

/// A retrospective/review finding to file under `learning/reviews`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ReviewEvent {
    pub title: String,
    pub description: String,
    pub body: String,
    pub tags: Vec<String>,
}

/// A journey milestone to file under `learning/journey`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct JourneyEvent {
    pub title: String,
    pub description: String,
    pub body: String,
}

/// A full replacement of the curator's single state document.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CuratorStateEvent {
    pub title: String,
    pub description: String,
    pub body: String,
}

/// Restore curator state from an earlier history snapshot.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RollbackEvent {
    pub snapshot_id: String,
    pub restored_concept_ids: Vec<String>,
}

/// Everything the queue knows how to apply to an agent's bundle.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum LearningEventPayload {
    Memory(MemoryLearningEvent),
    SkillUsage(SkillUsageEvent),
    Review(ReviewEvent),
    Journey(JourneyEvent),
    CuratorState(CuratorStateEvent),
    Rollback(RollbackEvent),
}

/// Durable queue façade over the store plus the per-agent knowledge
/// bundles the events are applied into.
pub struct LearningQueue {
    store: Arc<Store>,
    knowledge: Arc<AgentKnowledgeStore>,
    stale_after_ms: i64,
    #[cfg(test)]
    apply_pause: std::sync::Mutex<Option<(Arc<tokio::sync::Notify>, Arc<tokio::sync::Notify>)>>,
    #[cfg(test)]
    fail_next_discard: std::sync::atomic::AtomicBool,
}

impl LearningQueue {
    pub fn new(store: Arc<Store>, knowledge: Arc<AgentKnowledgeStore>) -> Self {
        Self {
            store,
            knowledge,
            stale_after_ms: DEFAULT_STALE_AFTER_MS,
            #[cfg(test)]
            apply_pause: std::sync::Mutex::new(None),
            #[cfg(test)]
            fail_next_discard: std::sync::atomic::AtomicBool::new(false),
        }
    }

    /// Durably appends one event for `agent_id`, allocating the next
    /// per-agent sequence inside a single Immediate transaction so
    /// concurrent enqueuers can never observe or produce a gap or a
    /// duplicate. Rejected outright when the agent is enqueue-blocked
    /// (it is being deleted).
    pub async fn enqueue(
        &self,
        agent_id: &str,
        payload: LearningEventPayload,
    ) -> anyhow::Result<LearningEvent> {
        validate_agent_id(agent_id).map_err(|issue| anyhow!(issue.message))?;
        let payload_json =
            serde_json::to_string(&payload).context("failed to serialize learning payload")?;
        let event_id = paths::new_id();
        let now = paths::now_ms();
        let sequence = self
            .store
            .with_conn({
                let event_id = event_id.clone();
                let agent = agent_id.to_owned();
                move |c| {
                    let tx = c.transaction_with_behavior(TransactionBehavior::Immediate)?;
                    tx.execute(
                        "INSERT OR IGNORE INTO agent_learning_state(agent_id) VALUES (?1)",
                        params![agent],
                    )?;
                    let (sequence, blocked): (i64, i64) = tx.query_row(
                        "SELECT next_sequence, enqueue_blocked \
                         FROM agent_learning_state WHERE agent_id=?1",
                        params![agent],
                        |r| Ok((r.get(0)?, r.get(1)?)),
                    )?;
                    if blocked != 0 {
                        return Err(queue_err(format!(
                            "agent `{agent}` no longer accepts learning events"
                        )));
                    }
                    tx.execute(
                        "UPDATE agent_learning_state SET next_sequence = next_sequence + 1 \
                         WHERE agent_id=?1",
                        params![agent],
                    )?;
                    tx.execute(
                        "INSERT INTO agent_learning_queue\
                         (event_id, agent_id, sequence, payload, status, attempts, created_at) \
                         VALUES (?1, ?2, ?3, ?4, 'pending', 0, ?5)",
                        params![event_id, agent, sequence, payload_json, now],
                    )?;
                    tx.commit()?;
                    Ok(sequence)
                }
            })
            .await?;
        Ok(LearningEvent {
            event_id,
            agent_id: agent_id.to_owned(),
            sequence,
            payload,
            attempts: 0,
        })
    }

    /// Claims the agent's lowest non-delivered event for `worker_id`.
    /// Stale claims for this agent are reset first; if the head event is
    /// still validly claimed by someone else this returns `None` — never
    /// a later sequence, so per-agent ordering is strict. Claiming
    /// increments `attempts`.
    pub async fn claim_next(
        &self,
        agent_id: &str,
        worker_id: &str,
    ) -> anyhow::Result<Option<LearningEvent>> {
        validate_agent_id(agent_id).map_err(|issue| anyhow!(issue.message))?;
        if worker_id.trim().is_empty() {
            bail!("worker id must not be blank");
        }
        let now = paths::now_ms();
        let stale_cutoff = now.saturating_sub(self.stale_after_ms);
        let row = self
            .store
            .with_conn({
                let agent = agent_id.to_owned();
                let worker = worker_id.to_owned();
                move |c| {
                    let tx = c.transaction_with_behavior(TransactionBehavior::Immediate)?;
                    let blocked: Option<i64> = tx
                        .query_row(
                            "SELECT enqueue_blocked FROM agent_learning_state WHERE agent_id=?1",
                            params![agent],
                            |r| r.get(0),
                        )
                        .optional()?;
                    if blocked == Some(1) {
                        tx.commit()?;
                        return Ok(None);
                    }
                    tx.execute(
                        "UPDATE agent_learning_queue \
                         SET status='pending', claimed_by=NULL, claimed_at=NULL \
                         WHERE agent_id=?1 AND status='claimed' AND claimed_at <= ?2",
                        params![agent, stale_cutoff],
                    )?;
                    let head: Option<(String, i64, String, String)> = tx
                        .query_row(
                            "SELECT event_id, sequence, payload, status \
                             FROM agent_learning_queue \
                             WHERE agent_id=?1 AND status IN ('pending','claimed') \
                             ORDER BY sequence LIMIT 1",
                            params![agent],
                            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
                        )
                        .optional()?;
                    let Some((event_id, sequence, payload, status)) = head else {
                        tx.commit()?;
                        return Ok(None);
                    };
                    if status != "pending" {
                        // The head is validly claimed elsewhere. Skipping
                        // ahead would break strict ordering; wait instead.
                        tx.commit()?;
                        return Ok(None);
                    }
                    let changed = tx.execute(
                        "UPDATE agent_learning_queue \
                         SET status='claimed', claimed_by=?2, claimed_at=?3, \
                             attempts = attempts + 1 \
                         WHERE event_id=?1 AND status='pending'",
                        params![event_id, worker, now],
                    )?;
                    if changed != 1 {
                        tx.commit()?;
                        return Ok(None);
                    }
                    let attempts: i64 = tx.query_row(
                        "SELECT attempts FROM agent_learning_queue WHERE event_id=?1",
                        params![event_id],
                        |r| r.get(0),
                    )?;
                    tx.commit()?;
                    Ok(Some((event_id, sequence, payload, attempts)))
                }
            })
            .await?;
        let Some((event_id, sequence, payload_json, attempts)) = row else {
            return Ok(None);
        };
        let payload: LearningEventPayload = serde_json::from_str(&payload_json)
            .with_context(|| format!("learning event `{event_id}` has an unreadable payload"))?;
        Ok(Some(LearningEvent {
            event_id,
            agent_id: agent_id.to_owned(),
            sequence,
            payload,
            attempts: u32::try_from(attempts).unwrap_or(u32::MAX),
        }))
    }

    /// Distinct agents with at least one pending event, ordered by each
    /// agent's oldest head-of-line pending event so the worker loop
    /// services the longest-waiting agent first.
    pub async fn pending_agents(&self) -> anyhow::Result<Vec<String>> {
        self.store
            .with_conn(|c| {
                let mut stmt = c.prepare(
                    "SELECT q.agent_id FROM agent_learning_queue q \
                     WHERE q.status='pending' \
                       AND q.sequence = (SELECT MIN(sequence) FROM agent_learning_queue \
                                         WHERE agent_id = q.agent_id AND status='pending') \
                     ORDER BY q.created_at, q.agent_id",
                )?;
                let rows = stmt
                    .query_map([], |r| r.get(0))?
                    .collect::<rusqlite::Result<Vec<String>>>();
                rows
            })
            .await
    }

    /// Applies a claimed event into the agent's OKF bundle. Idempotent:
    /// the knowledge store holds the per-agent lock across the recorded
    /// event-id check, the writes, index regeneration, and log append,
    /// and replaying an already-recorded event is a converging no-op.
    pub async fn apply_claimed(&self, event: &LearningEvent) -> anyhow::Result<()> {
        let fence = self.knowledge.write_fence(&event.agent_id)?;
        let _guard = fence.lock().await;
        if self.is_blocked(&event.agent_id).await? {
            bail!(
                "agent `{}` no longer accepts learning events",
                event.agent_id
            );
        }
        #[cfg(test)]
        let pause = { self.apply_pause.lock().unwrap().take() };
        #[cfg(test)]
        if let Some((entered, release)) = pause {
            entered.notify_one();
            release.notified().await;
        }
        let store = self.knowledge.for_agent(&event.agent_id)?;
        let agent_id = event.agent_id.clone();
        let event_id = event.event_id.clone();
        let payload = event.payload.clone();
        store.apply_learning_event_fenced(&event.event_id, move |scan| {
            plan_learning_writes(&agent_id, &event_id, &payload, scan)
        })?;
        Ok(())
    }

    pub(crate) async fn acquire_apply_fence(
        &self,
        agent_id: &str,
    ) -> anyhow::Result<tokio::sync::OwnedMutexGuard<()>> {
        Ok(self.knowledge.write_fence(agent_id)?.lock_owned().await)
    }

    async fn is_blocked(&self, agent_id: &str) -> anyhow::Result<bool> {
        let agent = agent_id.to_owned();
        self.store
            .with_conn(move |c| {
                c.query_row(
                    "SELECT enqueue_blocked FROM agent_learning_state WHERE agent_id=?1",
                    params![agent],
                    |row| row.get::<_, i64>(0),
                )
                .optional()
                .map(|blocked| blocked.unwrap_or(0) != 0)
            })
            .await
    }

    /// Acknowledges a claimed event as durably applied. Re-acknowledging
    /// an already-delivered event converges instead of failing.
    pub async fn mark_delivered(&self, event_id: &str) -> anyhow::Result<()> {
        let now = paths::now_ms();
        let event = event_id.to_owned();
        self.store
            .with_conn(move |c| {
                let changed = c.execute(
                    "UPDATE agent_learning_queue \
                     SET status='delivered', delivered_at=?2, claimed_by=NULL, claimed_at=NULL \
                     WHERE event_id=?1 AND status='claimed'",
                    params![event, now],
                )?;
                if changed == 1 {
                    return Ok(());
                }
                let status: Option<String> = c
                    .query_row(
                        "SELECT status FROM agent_learning_queue WHERE event_id=?1",
                        params![event],
                        |r| r.get(0),
                    )
                    .optional()?;
                match status.as_deref() {
                    Some("delivered") => Ok(()),
                    Some(other) => Err(queue_err(format!(
                        "learning event `{event}` is `{other}`, not claimed"
                    ))),
                    None => Err(queue_err(format!("learning event `{event}` was not found"))),
                }
            })
            .await
    }

    /// Returns a claimed event to `pending` after a failure, recording a
    /// truncated diagnostic. The claim-time attempt counter is the retry
    /// record; release never rewinds it.
    pub async fn release(&self, event_id: &str, error: &str) -> anyhow::Result<()> {
        let event = event_id.to_owned();
        let error = truncate_chars(error, MAX_LAST_ERROR_CHARS);
        self.store
            .with_conn(move |c| {
                let changed = c.execute(
                    "UPDATE agent_learning_queue \
                     SET status='pending', claimed_by=NULL, claimed_at=NULL, last_error=?2 \
                     WHERE event_id=?1 AND status='claimed'",
                    params![event, error],
                )?;
                if changed == 1 {
                    Ok(())
                } else {
                    Err(queue_err(format!(
                        "learning event `{event}` is not claimed, nothing to release"
                    )))
                }
            })
            .await
    }

    /// Resets every claim older than the stale window (relative to
    /// `now_ms`) across all agents; returns how many claims were reset.
    pub async fn reclaim_stale(&self, now_ms: i64) -> anyhow::Result<u64> {
        let stale_cutoff = now_ms.saturating_sub(self.stale_after_ms);
        self.store
            .with_conn(move |c| {
                let changed = c.execute(
                    "UPDATE agent_learning_queue \
                     SET status='pending', claimed_by=NULL, claimed_at=NULL \
                     WHERE status='claimed' AND claimed_at <= ?1",
                    params![stale_cutoff],
                )?;
                Ok(changed as u64)
            })
            .await
    }

    /// Returns every agent whose enqueue gate is blocked.
    pub async fn blocked_agents(&self) -> anyhow::Result<Vec<String>> {
        self.store
            .with_conn(|c| {
                let mut statement = c.prepare(
                    "SELECT agent_id FROM agent_learning_state WHERE enqueue_blocked=1 ORDER BY agent_id",
                )?;
                let rows = statement.query_map([], |row| row.get(0))?;
                rows.collect::<rusqlite::Result<Vec<String>>>()
            })
            .await
    }

    /// Stops accepting new events for the agent (deletion in progress).
    pub async fn block(&self, agent_id: &str) -> anyhow::Result<()> {
        self.set_blocked(agent_id, true).await
    }

    /// Re-opens the agent for new events.
    pub async fn unblock(&self, agent_id: &str) -> anyhow::Result<()> {
        self.set_blocked(agent_id, false).await
    }

    /// Drops every pending or claimed row for the agent; delivered rows
    /// stay as the audit trail of what was actually applied.
    pub async fn discard_unconsumed(&self, agent_id: &str) -> anyhow::Result<()> {
        #[cfg(test)]
        if self
            .fail_next_discard
            .swap(false, std::sync::atomic::Ordering::SeqCst)
        {
            bail!("injected learning queue discard failure");
        }
        validate_agent_id(agent_id).map_err(|issue| anyhow!(issue.message))?;
        let agent = agent_id.to_owned();
        self.store
            .with_conn(move |c| {
                c.execute(
                    "DELETE FROM agent_learning_queue \
                     WHERE agent_id=?1 AND status IN ('pending','claimed')",
                    params![agent],
                )
                .map(|_| ())
            })
            .await
    }

    #[cfg(test)]
    pub(crate) fn pause_next_apply_for_test(
        &self,
    ) -> (Arc<tokio::sync::Notify>, Arc<tokio::sync::Notify>) {
        let pause = (
            Arc::new(tokio::sync::Notify::new()),
            Arc::new(tokio::sync::Notify::new()),
        );
        *self.apply_pause.lock().unwrap() = Some(pause.clone());
        pause
    }

    #[cfg(test)]
    pub(crate) fn fail_next_discard_for_test(&self) {
        self.fail_next_discard
            .store(true, std::sync::atomic::Ordering::SeqCst);
    }

    async fn set_blocked(&self, agent_id: &str, blocked: bool) -> anyhow::Result<()> {
        validate_agent_id(agent_id).map_err(|issue| anyhow!(issue.message))?;
        let agent = agent_id.to_owned();
        let flag = i64::from(blocked);
        self.store
            .with_conn(move |c| {
                c.execute(
                    "INSERT INTO agent_learning_state(agent_id, enqueue_blocked) \
                     VALUES (?1, ?2) \
                     ON CONFLICT(agent_id) DO UPDATE SET enqueue_blocked=excluded.enqueue_blocked",
                    params![agent, flag],
                )
                .map(|_| ())
            })
            .await
    }
}

/// Domain error surfaced from inside a `with_conn` closure.
fn queue_err(message: String) -> rusqlite::Error {
    rusqlite::Error::ToSqlConversionFailure(message.into())
}

fn truncate_chars(value: &str, max: usize) -> String {
    value.chars().take(max).collect()
}

/// Maps one payload to the exact bundle writes/removes it produces.
/// Every written concept records `event_id`, which is both the
/// idempotency marker and the provenance trail.
fn plan_learning_writes(
    agent_id: &str,
    event_id: &str,
    payload: &LearningEventPayload,
    scan: &KnowledgeScan,
) -> anyhow::Result<LearningEventWrites> {
    match payload {
        LearningEventPayload::Memory(event) => plan_memory(agent_id, event_id, event, scan),
        LearningEventPayload::SkillUsage(event) => {
            plan_skill_usage(agent_id, event_id, event, scan)
        }
        LearningEventPayload::Review(event) => Ok(LearningEventWrites {
            writes: vec![render_event_concept(
                agent_id,
                event_id,
                &format!("learning/reviews/{event_id}.md"),
                "Review",
                &event.title,
                &event.description,
                &event.body,
                None,
                event.tags.clone(),
                IndexMap::new(),
            )?],
            removes: Vec::new(),
        }),
        LearningEventPayload::Journey(event) => Ok(LearningEventWrites {
            writes: vec![render_event_concept(
                agent_id,
                event_id,
                &format!("learning/journey/{event_id}.md"),
                "Journey",
                &event.title,
                &event.description,
                &event.body,
                None,
                Vec::new(),
                IndexMap::new(),
            )?],
            removes: Vec::new(),
        }),
        LearningEventPayload::CuratorState(event) => Ok(LearningEventWrites {
            writes: vec![render_event_concept(
                agent_id,
                event_id,
                "learning/curator/state.md",
                "CuratorState",
                &event.title,
                &event.description,
                &event.body,
                None,
                Vec::new(),
                IndexMap::new(),
            )?],
            removes: Vec::new(),
        }),
        LearningEventPayload::Rollback(event) => plan_rollback(agent_id, event_id, event, scan),
    }
}

fn plan_memory(
    agent_id: &str,
    event_id: &str,
    event: &MemoryLearningEvent,
    scan: &KnowledgeScan,
) -> anyhow::Result<LearningEventWrites> {
    let scope = match &event.operation {
        MemoryOperation::Add { scope, .. }
        | MemoryOperation::Replace { scope, .. }
        | MemoryOperation::Remove { scope, .. } => *scope,
    };
    let knowledge_scope = match scope {
        MemoryScope::Global => KnowledgeScope::Global,
        MemoryScope::User => KnowledgeScope::User,
        MemoryScope::Project => KnowledgeScope::Project {
            project_id: event
                .project_id
                .clone()
                .context("project-scope memory event carries no project id")?,
        },
    };
    let directory = ConceptArea::Memory(knowledge_scope.clone()).directory()?;
    let prefix = format!("{directory}/");
    let mut existing: Vec<&KnowledgeConcept> = scan
        .valid
        .iter()
        .filter(|concept| concept.relative_path.starts_with(&prefix))
        .collect();
    existing.sort_by(|a, b| (a.timestamp, &a.id).cmp(&(b.timestamp, &b.id)));

    match &event.operation {
        MemoryOperation::Add { text, .. } => {
            let text = text.trim();
            if text.is_empty() {
                bail!("memory add: `text` must not be empty");
            }
            let mut bodies: Vec<String> = existing.iter().map(|c| c.body.clone()).collect();
            bodies.push(text.to_owned());
            validate_budget(scope, &bodies)?;
            let sentence = first_sentence(text);
            Ok(LearningEventWrites {
                writes: vec![render_event_concept(
                    agent_id,
                    event_id,
                    &format!("{directory}/{}.md", paths::new_id()),
                    "Memory",
                    &truncate_chars(sentence, 80),
                    &truncate_chars(sentence, 160),
                    text,
                    Some(knowledge_scope),
                    Vec::new(),
                    IndexMap::new(),
                )?],
                removes: Vec::new(),
            })
        }
        MemoryOperation::Replace { matcher, text, .. } => {
            let text = text.trim();
            if text.is_empty() {
                bail!("memory replace: `text` must not be empty");
            }
            let target = find_unique_memory(&existing, matcher)?;
            let bodies: Vec<String> = existing
                .iter()
                .map(|c| {
                    if c.relative_path == target.relative_path {
                        text.to_owned()
                    } else {
                        c.body.clone()
                    }
                })
                .collect();
            validate_budget(scope, &bodies)?;
            let sentence = first_sentence(text);
            Ok(LearningEventWrites {
                writes: vec![render_event_concept(
                    agent_id,
                    event_id,
                    &target.relative_path,
                    "Memory",
                    &truncate_chars(sentence, 80),
                    &truncate_chars(sentence, 160),
                    text,
                    Some(knowledge_scope),
                    Vec::new(),
                    IndexMap::new(),
                )?],
                removes: Vec::new(),
            })
        }
        MemoryOperation::Remove { matcher, .. } => {
            let target = find_unique_memory(&existing, matcher)?;
            Ok(LearningEventWrites {
                writes: Vec::new(),
                removes: vec![target.relative_path.clone()],
            })
        }
    }
}

fn plan_skill_usage(
    agent_id: &str,
    event_id: &str,
    event: &SkillUsageEvent,
    scan: &KnowledgeScan,
) -> anyhow::Result<LearningEventWrites> {
    let stable = stable_skill_id(&event.skill_id)?;
    let relative_path = format!("learning/skills/{stable}.md");
    let existing = scan
        .valid
        .iter()
        .find(|concept| concept.relative_path == relative_path);
    let uses = existing.map_or(0, |c| extension_u64(c, "uses")) + 1;
    let successes =
        existing.map_or(0, |c| extension_u64(c, "successes")) + u64::from(event.succeeded);
    let mut extensions = IndexMap::new();
    extensions.insert("skill_id".into(), Value::String(event.skill_id.clone()));
    extensions.insert("uses".into(), Value::Number(uses.into()));
    extensions.insert("successes".into(), Value::Number(successes.into()));
    let title = truncate_chars(&format!("Skill usage: {}", event.skill_id), 80);
    Ok(LearningEventWrites {
        writes: vec![render_event_concept(
            agent_id,
            event_id,
            &relative_path,
            "Skill",
            &title,
            &truncate_chars(
                &format!("Usage counters for skill `{}`.", event.skill_id),
                160,
            ),
            "Usage counters for this skill are tracked in frontmatter.",
            None,
            Vec::new(),
            extensions,
        )?],
        removes: Vec::new(),
    })
}

fn plan_rollback(
    agent_id: &str,
    event_id: &str,
    event: &RollbackEvent,
    scan: &KnowledgeScan,
) -> anyhow::Result<LearningEventWrites> {
    validate_path_component(&event.snapshot_id).context("invalid rollback snapshot id")?;
    let snapshot_path = format!("learning/curator-history/{}.md", event.snapshot_id);
    let snapshot = scan
        .valid
        .iter()
        .find(|concept| concept.relative_path == snapshot_path)
        .with_context(|| format!("rollback snapshot `{}` was not found", event.snapshot_id))?;
    let mut extensions = IndexMap::new();
    extensions.insert(
        "snapshot_id".into(),
        Value::String(event.snapshot_id.clone()),
    );
    extensions.insert(
        "restored_concept_ids".into(),
        Value::Sequence(
            event
                .restored_concept_ids
                .iter()
                .cloned()
                .map(Value::String)
                .collect(),
        ),
    );
    let record_body = format!(
        "Rolled curator state back to snapshot `{}` ({} concept(s) restored).",
        event.snapshot_id,
        event.restored_concept_ids.len()
    );
    Ok(LearningEventWrites {
        writes: vec![
            // The restoration itself: curator state becomes the snapshot.
            render_event_concept(
                agent_id,
                event_id,
                "learning/curator/state.md",
                "CuratorState",
                &snapshot.title,
                &snapshot.description,
                &snapshot.body,
                None,
                Vec::new(),
                IndexMap::new(),
            )?,
            // The history record documenting that this rollback happened.
            render_event_concept(
                agent_id,
                event_id,
                &format!("learning/curator-history/{event_id}.md"),
                "CuratorHistory",
                &truncate_chars(&format!("Rollback to {}", event.snapshot_id), 80),
                &truncate_chars(&record_body, 160),
                &record_body,
                None,
                Vec::new(),
                extensions,
            )?,
        ],
        removes: Vec::new(),
    })
}

/// Renders one event-tagged OKF document; `(relative_path, markdown)`.
#[allow(clippy::too_many_arguments)]
fn render_event_concept(
    agent_id: &str,
    event_id: &str,
    relative_path: &str,
    concept_type: &str,
    title: &str,
    description: &str,
    body: &str,
    scope: Option<KnowledgeScope>,
    tags: Vec<String>,
    extensions: IndexMap<String, Value>,
) -> anyhow::Result<(String, String)> {
    if title.trim().is_empty() {
        bail!("learning concept title must not be blank");
    }
    if description.trim().is_empty() {
        bail!("learning concept description must not be blank");
    }
    let file_name = relative_path.rsplit('/').next().unwrap_or(relative_path);
    let id = file_name
        .strip_suffix(".md")
        .unwrap_or(file_name)
        .to_owned();
    let timestamp = DateTime::from_timestamp(Utc::now().timestamp(), 0)
        .context("system clock is out of range")?;
    let concept = KnowledgeConcept {
        id,
        relative_path: relative_path.to_owned(),
        concept_type: concept_type.to_owned(),
        title: title.to_owned(),
        description: description.to_owned(),
        timestamp,
        body: body.to_owned(),
        scope,
        agent_id: Some(agent_id.to_owned()),
        event_id: Some(event_id.to_owned()),
        tags,
        extensions,
    };
    Ok((relative_path.to_owned(), render_concept(&concept)?))
}

fn first_sentence(text: &str) -> &str {
    text.lines()
        .find(|line| !line.trim().is_empty())
        .unwrap_or(text)
        .trim()
}

fn find_unique_memory<'a>(
    concepts: &[&'a KnowledgeConcept],
    matcher: &str,
) -> anyhow::Result<&'a KnowledgeConcept> {
    if matcher.trim().is_empty() {
        bail!("memory: `match` must not be empty");
    }
    let hits: Vec<&&KnowledgeConcept> = concepts
        .iter()
        .filter(|concept| concept.body.contains(matcher))
        .collect();
    match hits.as_slice() {
        [one] => Ok(**one),
        [] => bail!("memory: no entry contains `{matcher}`"),
        many => bail!(
            "memory: `{matcher}` matches {} entries — use a longer, unique substring",
            many.len()
        ),
    }
}

fn extension_u64(concept: &KnowledgeConcept, key: &str) -> u64 {
    match concept.extensions.get(key) {
        Some(Value::Number(value)) => value.as_u64().unwrap_or(0),
        _ => 0,
    }
}

/// A skill id reduced to one safe path component. Unsafe bytes become
/// `-`; ids that would collide with generated file names get a prefix.
fn stable_skill_id(skill_id: &str) -> anyhow::Result<String> {
    let trimmed = skill_id.trim();
    if trimmed.is_empty() {
        bail!("skill id must not be blank");
    }
    let mut stable: String = trimmed
        .chars()
        .take(100)
        .map(|c| {
            if c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-') {
                c.to_ascii_lowercase()
            } else {
                '-'
            }
        })
        .collect();
    if RESERVED_FILE_NAMES.contains(&format!("{stable}.md").as_str())
        || stable == "."
        || stable == ".."
    {
        stable = format!("skill-{stable}");
    }
    validate_path_component(&stable)
        .with_context(|| format!("skill id `{skill_id}` cannot form a safe file name"))?;
    Ok(stable)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn memory_event(text: &str) -> MemoryLearningEvent {
        MemoryLearningEvent {
            operation: MemoryOperation::Add {
                scope: MemoryScope::User,
                text: text.to_owned(),
            },
            source: "test".into(),
            project_id: None,
        }
    }

    fn review_payload(title: &str) -> LearningEventPayload {
        LearningEventPayload::Review(ReviewEvent {
            title: title.to_owned(),
            description: format!("{title} description"),
            body: format!("{title} body"),
            tags: Vec::new(),
        })
    }

    async fn fixture_queue() -> (tempfile::TempDir, tempfile::TempDir, LearningQueue) {
        let root = tempfile::tempdir().unwrap();
        let db_dir = tempfile::tempdir().unwrap();
        let store = Arc::new(Store::open(&db_dir.path().join("store.db")).await.unwrap());
        let knowledge = Arc::new(AgentKnowledgeStore::new(root.path().to_path_buf()));
        let queue = LearningQueue::new(store, knowledge);
        (root, db_dir, queue)
    }

    async fn queue_status(queue: &LearningQueue, event_id: &str) -> String {
        queue
            .store
            .with_conn({
                let event_id = event_id.to_owned();
                move |c| {
                    c.query_row(
                        "SELECT status FROM agent_learning_queue WHERE event_id=?1",
                        params![event_id],
                        |row| row.get(0),
                    )
                }
            })
            .await
            .unwrap()
    }

    #[tokio::test]
    async fn worker_retries_failure_then_delivers_in_agent_sequence() {
        let (root, _tmp, queue) = fixture_queue().await;
        let queue = Arc::new(queue);
        let first = queue.enqueue("a", review_payload("first")).await.unwrap();
        let second = queue.enqueue("a", review_payload("second")).await.unwrap();
        let knowledge_path = root.path().join("agents/a/knowledge");
        std::fs::create_dir_all(knowledge_path.parent().unwrap()).unwrap();
        std::fs::write(&knowledge_path, "blocks bundle creation").unwrap();

        crate::learning::tick(&queue, "worker").await;
        assert_eq!(queue_status(&queue, &first.event_id).await, "pending");
        assert_eq!(queue_status(&queue, &second.event_id).await, "pending");

        std::fs::remove_file(knowledge_path).unwrap();
        crate::learning::tick(&queue, "worker").await;
        assert_eq!(queue_status(&queue, &first.event_id).await, "delivered");
        assert_eq!(queue_status(&queue, &second.event_id).await, "pending");
        crate::learning::tick(&queue, "worker").await;
        assert_eq!(queue_status(&queue, &second.event_id).await, "delivered");
    }

    #[tokio::test]
    async fn enqueue_allocates_monotonic_sequence_per_agent_atomically() {
        let (_root, _tmp, queue) = fixture_queue().await;
        let a1 = queue
            .enqueue("a", LearningEventPayload::Memory(memory_event("one")))
            .await
            .unwrap();
        let b1 = queue
            .enqueue("b", LearningEventPayload::Memory(memory_event("other")))
            .await
            .unwrap();
        let a2 = queue
            .enqueue("a", LearningEventPayload::Memory(memory_event("two")))
            .await
            .unwrap();
        assert_eq!((a1.sequence, b1.sequence, a2.sequence), (1, 1, 2));
    }

    #[tokio::test]
    async fn replayed_event_is_applied_once_and_then_acknowledged() {
        let (_root, _tmp, queue) = fixture_queue().await;
        let event = queue.enqueue("a", review_payload("finding")).await.unwrap();
        let claimed = queue.claim_next("a", "w1").await.unwrap().unwrap();
        queue.apply_claimed(&claimed).await.unwrap();
        // Crash window: applied but never acknowledged — the event goes
        // back to pending and a second worker replays it end to end.
        queue
            .release(&event.event_id, "simulated ack crash")
            .await
            .unwrap();
        let replay = queue.claim_next("a", "w2").await.unwrap().unwrap();
        assert_eq!(replay.event_id, event.event_id);
        assert_eq!(replay.attempts, 2);
        queue.apply_claimed(&replay).await.unwrap();
        queue.mark_delivered(&replay.event_id).await.unwrap();
        let concepts = queue
            .knowledge
            .for_agent("a")
            .unwrap()
            .scan()
            .await
            .unwrap()
            .valid;
        assert_eq!(
            concepts
                .iter()
                .filter(|c| c.event_id.as_deref() == Some(event.event_id.as_str()))
                .count(),
            1
        );
        assert!(queue.claim_next("a", "w2").await.unwrap().is_none());
    }

    #[tokio::test]
    async fn stale_claim_resumes_in_sequence_and_different_agents_progress() {
        let (_root, _tmp, queue) = fixture_queue().await;
        let a1 = queue.enqueue("a", review_payload("a1")).await.unwrap();
        queue.enqueue("a", review_payload("a2")).await.unwrap();
        queue.enqueue("b", review_payload("b1")).await.unwrap();
        assert_eq!(
            queue
                .claim_next("a", "dead")
                .await
                .unwrap()
                .unwrap()
                .event_id,
            a1.event_id
        );
        // Sequence 1 is claimed and not stale: no skipping to sequence 2.
        assert!(queue.claim_next("a", "other").await.unwrap().is_none());
        // Another agent is unaffected by agent a's stuck head.
        assert!(queue.claim_next("b", "other").await.unwrap().is_some());
        // Everything is stale relative to i64::MAX: both a's dead claim
        // and b's in-flight claim are reset.
        assert_eq!(queue.reclaim_stale(i64::MAX).await.unwrap(), 2);
        let resumed = queue.claim_next("a", "other").await.unwrap().unwrap();
        assert_eq!(resumed.sequence, 1);
        assert_eq!(resumed.event_id, a1.event_id);
    }

    #[tokio::test]
    async fn block_then_discard_prevents_new_events_and_removes_unconsumed_rows() {
        let (_root, _tmp, queue) = fixture_queue().await;
        queue.enqueue("a", review_payload("pending")).await.unwrap();
        queue.block("a").await.unwrap();
        queue.discard_unconsumed("a").await.unwrap();
        assert!(queue.enqueue("a", review_payload("late")).await.is_err());
        assert!(queue.claim_next("a", "worker").await.unwrap().is_none());
        assert!(queue.pending_agents().await.unwrap().is_empty());
        // Unblocking re-opens the agent with a fresh, still-monotonic tail.
        queue.unblock("a").await.unwrap();
        let next = queue.enqueue("a", review_payload("again")).await.unwrap();
        assert_eq!(next.sequence, 2);
    }

    #[tokio::test]
    async fn memory_event_applies_to_requested_scope_with_event_id() {
        let (_root, _tmp, queue) = fixture_queue().await;
        queue
            .enqueue("a", LearningEventPayload::Memory(memory_event("a fact")))
            .await
            .unwrap();
        let claimed = queue.claim_next("a", "w").await.unwrap().unwrap();
        queue.apply_claimed(&claimed).await.unwrap();
        queue.mark_delivered(&claimed.event_id).await.unwrap();
        let scan = queue
            .knowledge
            .for_agent("a")
            .unwrap()
            .scan()
            .await
            .unwrap();
        let facts: Vec<_> = scan
            .valid
            .iter()
            .filter(|c| c.relative_path.starts_with("memory/user/"))
            .collect();
        assert_eq!(facts.len(), 1);
        assert_eq!(facts[0].body, "a fact");
        assert_eq!(
            facts[0].event_id.as_deref(),
            Some(claimed.event_id.as_str())
        );
    }

    #[tokio::test]
    async fn skill_usage_accumulates_counters_in_one_stable_concept() {
        let (_root, _tmp, queue) = fixture_queue().await;
        for succeeded in [true, false, true] {
            let event = queue
                .enqueue(
                    "a",
                    LearningEventPayload::SkillUsage(SkillUsageEvent {
                        skill_id: "Commit Helper".into(),
                        succeeded,
                        source: "session".into(),
                    }),
                )
                .await
                .unwrap();
            let claimed = queue.claim_next("a", "w").await.unwrap().unwrap();
            assert_eq!(claimed.event_id, event.event_id);
            queue.apply_claimed(&claimed).await.unwrap();
            queue.mark_delivered(&claimed.event_id).await.unwrap();
        }
        let snapshot = queue.knowledge.learning_snapshot("a").await.unwrap();
        assert_eq!(snapshot.skill_usage.len(), 1);
        assert_eq!(snapshot.skill_usage[0].skill_id, "Commit Helper");
        assert_eq!(snapshot.skill_usage[0].uses, 3);
        assert_eq!(snapshot.skill_usage[0].successes, 2);
    }

    #[tokio::test]
    async fn pending_agents_orders_by_oldest_head_of_line_event() {
        let (_root, _tmp, queue) = fixture_queue().await;
        let b = queue.enqueue("b", review_payload("b1")).await.unwrap();
        let a = queue.enqueue("a", review_payload("a1")).await.unwrap();
        // Pin creation times so ordering is deterministic under a fast clock.
        queue
            .store
            .with_conn({
                let (a_id, b_id) = (a.event_id.clone(), b.event_id.clone());
                move |c| {
                    c.execute(
                        "UPDATE agent_learning_queue SET created_at=100 WHERE event_id=?1",
                        params![b_id],
                    )?;
                    c.execute(
                        "UPDATE agent_learning_queue SET created_at=200 WHERE event_id=?1",
                        params![a_id],
                    )?;
                    Ok(())
                }
            })
            .await
            .unwrap();
        assert_eq!(queue.pending_agents().await.unwrap(), vec!["b", "a"]);
    }

    #[tokio::test]
    async fn release_truncates_error_and_rollback_restores_snapshot() {
        let (_root, _tmp, queue) = fixture_queue().await;
        // Seed curator state + a history snapshot, then roll back to it.
        queue
            .enqueue(
                "a",
                LearningEventPayload::CuratorState(CuratorStateEvent {
                    title: "Original state".into(),
                    description: "Original description.".into(),
                    body: "Original body.".into(),
                }),
            )
            .await
            .unwrap();
        let claimed = queue.claim_next("a", "w").await.unwrap().unwrap();
        queue.apply_claimed(&claimed).await.unwrap();
        queue.mark_delivered(&claimed.event_id).await.unwrap();
        let snapshot_id = claimed.event_id.clone();
        // Manually place a history snapshot carrying the state to restore.
        let knowledge = queue.knowledge.for_agent("a").unwrap();
        let (path, markdown) = render_event_concept(
            "a",
            &snapshot_id,
            &format!("learning/curator-history/{snapshot_id}.md"),
            "CuratorHistory",
            "Snapshotted state",
            "State before curation.",
            "Snapshot body.",
            None,
            Vec::new(),
            IndexMap::new(),
        )
        .unwrap();
        knowledge.replace_raw(&path, &markdown).await.unwrap();
        let rollback = queue
            .enqueue(
                "a",
                LearningEventPayload::Rollback(RollbackEvent {
                    snapshot_id: snapshot_id.clone(),
                    restored_concept_ids: vec!["state".into()],
                }),
            )
            .await
            .unwrap();
        let claimed = queue.claim_next("a", "w").await.unwrap().unwrap();
        // A failed apply releases with a huge error; it must be truncated.
        queue
            .release(&claimed.event_id, &"x".repeat(2000))
            .await
            .unwrap();
        let stored_error: String = queue
            .store
            .with_conn({
                let event_id = claimed.event_id.clone();
                move |c| {
                    c.query_row(
                        "SELECT last_error FROM agent_learning_queue WHERE event_id=?1",
                        params![event_id],
                        |r| r.get(0),
                    )
                }
            })
            .await
            .unwrap();
        assert_eq!(stored_error.chars().count(), 500);
        let retried = queue.claim_next("a", "w").await.unwrap().unwrap();
        assert_eq!(retried.event_id, rollback.event_id);
        queue.apply_claimed(&retried).await.unwrap();
        queue.mark_delivered(&retried.event_id).await.unwrap();
        let scan = knowledge.scan().await.unwrap();
        let state = scan
            .valid
            .iter()
            .find(|c| c.relative_path == "learning/curator/state.md")
            .unwrap();
        assert_eq!(state.title, "Snapshotted state");
        assert_eq!(state.event_id.as_deref(), Some(rollback.event_id.as_str()));
        assert!(scan.valid.iter().any(
            |c| c.relative_path == format!("learning/curator-history/{}.md", rollback.event_id)
        ));
    }
}
