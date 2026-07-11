//! Auto decomposition: the orchestrated task graph.
//!
//! Ported from hermes-agent's kanban decomposer, rebuilt on ryuzi's session
//! machinery: a goal (root task) is decomposed by an LLM into 2–6
//! self-contained subtasks with dependency edges; a dispatcher loop promotes
//! tasks whose parents are done, runs each as its own agent session (bounded
//! by `max_concurrent_runs`), and finally wakes a judge session on the root to
//! synthesize the outcome. Schema lives in `store.rs` (`orch_tasks`,
//! `orch_task_deps`); this module keeps its SQL beside the logic via
//! [`Store::with_conn`], per the store's sanctioned pattern.
//!
//! Root lifecycle: `decomposing → waiting → judging → done|failed` (a
//! non-decomposed submit starts at `waiting`). Child lifecycle:
//! `todo → ready → running → done|failed|cancelled`.

use crate::control::ControlPlane;
use crate::domain::CoreEvent;
use crate::store::Store;
use rusqlite::{params, OptionalExtension};
use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::broadcast;

/// One row of the orchestrated task graph.
#[derive(Debug, Clone, PartialEq)]
pub struct OrchTask {
    pub id: String,
    /// `None` for a root (goal) task.
    pub root_id: Option<String>,
    pub project_id: String,
    pub title: String,
    pub body: String,
    /// Recorded from the decomposer and resolved by name against
    /// `AgentRegistry` when the worker session starts (falling back to the
    /// registry's default agent for an unknown/blank name).
    pub agent: String,
    pub status: String,
    pub session_pk: Option<String>,
    pub result: Option<String>,
    pub error: Option<String>,
    pub created_at: i64,
    pub finished_at: Option<i64>,
    /// The originating chat session (root only) — where worker bubbles post
    /// and the aggregate outcome re-enters over the rail. `None` for goals
    /// submitted without a home chat (CLI/tests).
    pub home_session_pk: Option<String>,
    /// Consecutive failed attempts for this child (circuit breaker input).
    pub consecutive_failures: i64,
    /// The breaker tripped: this child exhausted its retries and stays failed.
    pub gave_up: bool,
    /// Accumulated mid-run user guidance (root only), fed to the judge prompt.
    pub steer_note: Option<String>,
}

/// One subtask planned by the decomposer, before insertion.
#[derive(Debug, Clone, PartialEq)]
pub struct PlannedTask {
    pub title: String,
    pub body: String,
    pub agent: String,
    /// Indices into the same plan array; each parent must finish first.
    pub parents: Vec<usize>,
}

// ---------------------------------------------------------------------------
// Decomposition contract
// ---------------------------------------------------------------------------

/// The one-shot prompt asking the model to decompose `goal` (hermes-agent's
/// fanout contract: strict JSON, 2–6 self-contained tasks, roster-restricted
/// assignees, parents by index).
pub fn decomposition_prompt(goal: &str, roster: &[String]) -> String {
    format!(
        "Decompose the following goal into 2-6 subtasks.\n\
         Reply with ONLY a JSON object, no prose, of the form:\n\
         {{\"tasks\": [{{\"title\": \"short name\", \"body\": \"full self-contained \
         instructions\", \"agent\": \"one of: {}\", \"parents\": [indices of tasks \
         in this array that must finish first]}}]}}\n\
         Rules:\n\
         - Each body must be fully self-contained: the worker sees NOTHING else \
         (no goal text, no sibling tasks), so repeat any needed context.\n\
         - Use parents only for real data dependencies; independent tasks run in \
         parallel.\n\
         - No dependency cycles. parents refer to array positions (0-based).\n\n\
         Goal:\n{goal}",
        roster.join(", "),
    )
}

/// Parse + validate a decomposition reply: fence-stripping, strict shape,
/// parents in range, and a Kahn cycle check. Unknown agents degrade to
/// `build`; structural problems are errors (the root is then failed).
pub fn parse_decomposition(raw: &str, roster: &[String]) -> anyhow::Result<Vec<PlannedTask>> {
    let json = extract_json(raw);
    let v: serde_json::Value = serde_json::from_str(json.trim())
        .map_err(|e| anyhow::anyhow!("decomposition is not valid JSON: {e}"))?;
    let tasks = v
        .get("tasks")
        .and_then(|t| t.as_array())
        .ok_or_else(|| anyhow::anyhow!("decomposition has no `tasks` array"))?;
    if !(2..=6).contains(&tasks.len()) {
        anyhow::bail!("decomposition must have 2-6 tasks, got {}", tasks.len());
    }
    let n = tasks.len();
    let mut planned = Vec::with_capacity(n);
    for (i, t) in tasks.iter().enumerate() {
        let title = t
            .get("title")
            .and_then(|s| s.as_str())
            .unwrap_or_default()
            .trim()
            .to_string();
        let body = t
            .get("body")
            .and_then(|s| s.as_str())
            .unwrap_or_default()
            .trim()
            .to_string();
        if title.is_empty() || body.is_empty() {
            anyhow::bail!("task {i} is missing a title or body");
        }
        let agent = t
            .get("agent")
            .and_then(|s| s.as_str())
            .filter(|a| roster.iter().any(|r| r == a))
            .unwrap_or("build")
            .to_string();
        let mut parents = Vec::new();
        if let Some(ps) = t.get("parents").and_then(|p| p.as_array()) {
            for p in ps {
                let idx = p
                    .as_u64()
                    .ok_or_else(|| anyhow::anyhow!("task {i} has a non-integer parent"))?
                    as usize;
                if idx >= n {
                    anyhow::bail!("task {i} has out-of-range parent {idx} (only {n} tasks)");
                }
                if idx == i {
                    anyhow::bail!("task {i} depends on itself");
                }
                parents.push(idx);
            }
        }
        planned.push(PlannedTask {
            title,
            body,
            agent,
            parents,
        });
    }
    check_acyclic(&planned)?;
    Ok(planned)
}

/// Strip markdown code fences / surrounding prose down to the JSON object.
fn extract_json(raw: &str) -> &str {
    let trimmed = raw.trim();
    // Prefer the outermost braces — robust against ``` fences and prose.
    match (trimmed.find('{'), trimmed.rfind('}')) {
        (Some(a), Some(b)) if b > a => &trimmed[a..=b],
        _ => trimmed,
    }
}

/// Kahn's algorithm over parent edges; errors when a cycle remains.
fn check_acyclic(tasks: &[PlannedTask]) -> anyhow::Result<()> {
    let n = tasks.len();
    let mut unmet: Vec<usize> = tasks.iter().map(|t| t.parents.len()).collect();
    let mut done = vec![false; n];
    let mut processed = 0;
    while let Some(next) = (0..n).find(|&i| !done[i] && unmet[i] == 0) {
        done[next] = true;
        processed += 1;
        for (i, t) in tasks.iter().enumerate() {
            if !done[i] && t.parents.contains(&next) {
                unmet[i] -= 1;
            }
        }
    }
    if processed < n {
        anyhow::bail!("decomposition has a dependency cycle");
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Persistence
// ---------------------------------------------------------------------------

const ORCH_COLS: &str =
    "id,root_id,project_id,title,body,agent,status,session_pk,result,error,created_at,finished_at,\
     home_session_pk,consecutive_failures,gave_up,steer_note";

fn task_from(r: &rusqlite::Row) -> rusqlite::Result<OrchTask> {
    Ok(OrchTask {
        id: r.get(0)?,
        root_id: r.get(1)?,
        project_id: r.get(2)?,
        title: r.get(3)?,
        body: r.get(4)?,
        agent: r.get(5)?,
        status: r.get(6)?,
        session_pk: r.get(7)?,
        result: r.get(8)?,
        error: r.get(9)?,
        created_at: r.get(10)?,
        finished_at: r.get(11)?,
        home_session_pk: r.get(12)?,
        consecutive_failures: r.get(13)?,
        gave_up: r.get::<_, i64>(14)? != 0,
        steer_note: r.get(15)?,
    })
}

fn new_task_id() -> String {
    format!("ot-{}", &crate::paths::new_id()[..8])
}

/// Insert a root (goal) task with `status` and return its id.
pub async fn insert_root(
    store: &Store,
    project_id: &str,
    goal: &str,
    status: &str,
    home_session_pk: Option<&str>,
) -> anyhow::Result<String> {
    let id = new_task_id();
    let title: String = goal.chars().take(80).collect();
    let (id2, project_id, goal, status, home, now) = (
        id.clone(),
        project_id.to_string(),
        goal.to_string(),
        status.to_string(),
        home_session_pk.map(str::to_string),
        crate::paths::now_ms(),
    );
    store
        .with_conn(move |c| {
            c.execute(
                "INSERT INTO orch_tasks(id,root_id,project_id,title,body,agent,status,created_at,home_session_pk) \
                 VALUES (?1,NULL,?2,?3,?4,'',?5,?6,?7)",
                params![id2, project_id, title, goal, status, now, home],
            )
            .map(|_| ())
        })
        .await?;
    Ok(id)
}

/// Insert a root's planned children (status `todo`) plus their dependency
/// edges in one transaction. Returns the child ids in plan order.
pub async fn insert_children(
    store: &Store,
    root_id: &str,
    project_id: &str,
    plan: &[PlannedTask],
) -> anyhow::Result<Vec<String>> {
    let ids: Vec<String> = plan.iter().map(|_| new_task_id()).collect();
    let (root_id, project_id, plan, ids2, now) = (
        root_id.to_string(),
        project_id.to_string(),
        plan.to_vec(),
        ids.clone(),
        crate::paths::now_ms(),
    );
    store
        .with_conn(move |c| {
            let tx = c.transaction()?;
            for (i, t) in plan.iter().enumerate() {
                tx.execute(
                    "INSERT INTO orch_tasks(id,root_id,project_id,title,body,agent,status,created_at) \
                     VALUES (?1,?2,?3,?4,?5,?6,'todo',?7)",
                    params![ids2[i], root_id, project_id, t.title, t.body, t.agent, now + i as i64],
                )?;
                for &p in &t.parents {
                    tx.execute(
                        "INSERT INTO orch_task_deps(task_id, dep_id) VALUES (?1, ?2)",
                        params![ids2[i], ids2[p]],
                    )?;
                }
            }
            tx.commit()?;
            Ok(())
        })
        .await?;
    Ok(ids)
}

/// All tasks (roots first, then children by creation), optionally filtered to
/// one root's tree.
pub async fn list_tasks(store: &Store, root: Option<&str>) -> anyhow::Result<Vec<OrchTask>> {
    let root = root.map(str::to_string);
    store
        .with_conn(move |c| {
            let mut out = Vec::new();
            match &root {
                Some(r) => {
                    let mut stmt = c.prepare(&format!(
                        "SELECT {ORCH_COLS} FROM orch_tasks WHERE id=?1 OR root_id=?1 \
                         ORDER BY root_id IS NOT NULL, created_at"
                    ))?;
                    let rows = stmt.query_map(params![r], task_from)?;
                    for row in rows {
                        out.push(row?);
                    }
                }
                None => {
                    let mut stmt = c.prepare(&format!(
                        "SELECT {ORCH_COLS} FROM orch_tasks \
                         ORDER BY root_id IS NOT NULL, created_at"
                    ))?;
                    let rows = stmt.query_map([], task_from)?;
                    for row in rows {
                        out.push(row?);
                    }
                }
            }
            Ok(out)
        })
        .await
}

pub async fn get_task(store: &Store, id: &str) -> anyhow::Result<Option<OrchTask>> {
    let id = id.to_string();
    store
        .with_conn(move |c| {
            c.query_row(
                &format!("SELECT {ORCH_COLS} FROM orch_tasks WHERE id=?1"),
                params![id],
                task_from,
            )
            .optional()
        })
        .await
}

/// The home chat of a task's root (workers post bubbles + deliver into it).
pub async fn home_session(store: &Store, task: &OrchTask) -> anyhow::Result<Option<String>> {
    let root_id = task.root_id.clone().unwrap_or_else(|| task.id.clone());
    Ok(get_task(store, &root_id)
        .await?
        .and_then(|r| r.home_session_pk))
}

/// Flip `todo` children whose dependencies are all `done` to `ready`.
/// Only children of a live (`waiting`) root promote — once a root fails or is
/// cancelled, its remaining subtasks must never spawn sessions.
/// Returns the promoted ids.
pub async fn promote_ready(store: &Store) -> anyhow::Result<Vec<String>> {
    store
        .with_conn(|c| {
            let tx = c.transaction()?;
            let promoted: Vec<String> = {
                let mut stmt = tx.prepare(
                    "SELECT id FROM orch_tasks t WHERE t.status='todo' AND EXISTS (\
                        SELECT 1 FROM orch_tasks r \
                        WHERE r.id = t.root_id AND r.status = 'waiting') \
                     AND NOT EXISTS (\
                        SELECT 1 FROM orch_task_deps d \
                        JOIN orch_tasks p ON p.id = d.dep_id \
                        WHERE d.task_id = t.id AND p.status != 'done')",
                )?;
                let rows = stmt.query_map([], |r| r.get::<_, String>(0))?;
                rows.collect::<rusqlite::Result<Vec<_>>>()?
            };
            for id in &promoted {
                tx.execute(
                    "UPDATE orch_tasks SET status='ready' WHERE id=?1",
                    params![id],
                )?;
            }
            tx.commit()?;
            Ok(promoted)
        })
        .await
}

/// Claim up to `cap - currently_running` `ready` tasks as `running`, oldest
/// first. The live-root guard is re-checked here so tasks promoted before
/// their root died are parked, not launched. Returns the claimed rows.
pub async fn claim_ready(store: &Store, cap: usize) -> anyhow::Result<Vec<OrchTask>> {
    store
        .with_conn(move |c| {
            let tx = c.transaction()?;
            let running: i64 = tx.query_row(
                "SELECT count(*) FROM orch_tasks WHERE status='running'",
                [],
                |r| r.get(0),
            )?;
            let slots = (cap as i64 - running).max(0);
            let claimed: Vec<OrchTask> = {
                let mut stmt = tx.prepare(&format!(
                    "SELECT {ORCH_COLS} FROM orch_tasks t WHERE t.status='ready' \
                     AND EXISTS (SELECT 1 FROM orch_tasks r \
                        WHERE r.id = t.root_id AND r.status = 'waiting') \
                     ORDER BY t.created_at LIMIT ?1"
                ))?;
                let rows = stmt.query_map(params![slots], task_from)?;
                rows.collect::<rusqlite::Result<Vec<_>>>()?
            };
            for t in &claimed {
                tx.execute(
                    "UPDATE orch_tasks SET status='running' WHERE id=?1",
                    params![t.id],
                )?;
            }
            tx.commit()?;
            Ok(claimed)
        })
        .await
}

/// Record a task's session as soon as it starts (for UI linkage).
pub async fn set_task_session(store: &Store, id: &str, session_pk: &str) -> anyhow::Result<()> {
    let (id, session_pk) = (id.to_string(), session_pk.to_string());
    store
        .with_conn(move |c| {
            c.execute(
                "UPDATE orch_tasks SET session_pk=?2 WHERE id=?1",
                params![id, session_pk],
            )
            .map(|_| ())
        })
        .await
}

/// Move a task from `expected` to `status`, recording result/error. Returns
/// whether the row changed — a `false` means something else (e.g. a cancel)
/// won the race and the caller's outcome is discarded.
pub async fn finish_task(
    store: &Store,
    id: &str,
    expected: &str,
    status: &str,
    result: Option<String>,
    error: Option<String>,
) -> anyhow::Result<bool> {
    let (id, expected, status) = (id.to_string(), expected.to_string(), status.to_string());
    let now = crate::paths::now_ms();
    store
        .with_conn(move |c| {
            let n = c.execute(
                "UPDATE orch_tasks SET status=?3, result=?4, error=?5, finished_at=?6 \
                 WHERE id=?1 AND status=?2",
                params![id, expected, status, result, error, now],
            )?;
            Ok(n > 0)
        })
        .await
}

/// Cancel a task and (for roots) every unfinished child. Running sessions are
/// not killed — their results are discarded by the `finish_task` guard.
/// Returns how many rows were cancelled.
pub async fn cancel_tree(store: &Store, id: &str) -> anyhow::Result<u32> {
    let id = id.to_string();
    let now = crate::paths::now_ms();
    store
        .with_conn(move |c| {
            let n = c.execute(
                "UPDATE orch_tasks SET status='cancelled', finished_at=?2 \
                 WHERE (id=?1 OR root_id=?1) \
                 AND status IN ('todo','ready','running','waiting','judging','decomposing')",
                params![id, now],
            )?;
            Ok(n as u32)
        })
        .await
}

/// Re-queue failed work. For a failed child: the child goes back to `todo`
/// AND its failed root is revived to `waiting` (the dispatcher fails a root
/// within one tick of any child failing, so without the revival a retried
/// child's result could never be judged). For a failed root: every
/// failed/cancelled child re-queues and the root returns to `waiting`; a
/// childless failed root (decomposition failure) cannot be retried —
/// re-submit the goal instead. Roots never become `todo`: `todo` rows are
/// claimed as worker sessions, and a root must only ever run as a judge.
pub async fn retry_task(store: &Store, id: &str) -> anyhow::Result<bool> {
    let id = id.to_string();
    store
        .with_conn(move |c| {
            let tx = c.transaction()?;
            let row: Option<(Option<String>, String)> = tx
                .query_row(
                    "SELECT root_id, status FROM orch_tasks WHERE id=?1",
                    params![&id],
                    |r| Ok((r.get(0)?, r.get(1)?)),
                )
                .optional()?;
            let Some((root_id, status)) = row else {
                return Ok(false);
            };
            if status != "failed" {
                return Ok(false);
            }
            let changed = match root_id {
                // A failed child: re-queue it, re-queue siblings that were
                // cancelled as collateral when the root failed, and revive
                // the root itself.
                Some(root) => {
                    tx.execute(
                        "UPDATE orch_tasks SET status='todo', error=NULL, result=NULL, \
                         session_pk=NULL, finished_at=NULL WHERE id=?1",
                        params![&id],
                    )?;
                    tx.execute(
                        "UPDATE orch_tasks SET status='todo', error=NULL, result=NULL, \
                         session_pk=NULL, finished_at=NULL \
                         WHERE root_id=?1 AND status='cancelled'",
                        params![&root],
                    )?;
                    tx.execute(
                        "UPDATE orch_tasks SET status='waiting', error=NULL, \
                         finished_at=NULL WHERE id=?1 AND status='failed'",
                        params![root],
                    )?;
                    true
                }
                // A failed root: re-queue its failed/cancelled children.
                None => {
                    let children: i64 = tx.query_row(
                        "SELECT count(*) FROM orch_tasks WHERE root_id=?1",
                        params![&id],
                        |r| r.get(0),
                    )?;
                    if children == 0 {
                        false // decomposition failure — nothing to re-run
                    } else {
                        tx.execute(
                            "UPDATE orch_tasks SET status='todo', error=NULL, result=NULL, \
                             session_pk=NULL, finished_at=NULL \
                             WHERE root_id=?1 AND status IN ('failed','cancelled')",
                            params![&id],
                        )?;
                        tx.execute(
                            "UPDATE orch_tasks SET status='waiting', error=NULL, \
                             finished_at=NULL WHERE id=?1",
                            params![&id],
                        )?;
                        true
                    }
                }
            };
            tx.commit()?;
            Ok(changed)
        })
        .await
}

/// `waiting` roots whose children (at least one) are all `done` — ready for a
/// judge session.
pub async fn roots_ready_to_judge(store: &Store) -> anyhow::Result<Vec<OrchTask>> {
    store
        .with_conn(|c| {
            let mut stmt = c.prepare(&format!(
                "SELECT {ORCH_COLS} FROM orch_tasks r WHERE r.root_id IS NULL \
                 AND r.status='waiting' \
                 AND EXISTS (SELECT 1 FROM orch_tasks ch WHERE ch.root_id = r.id) \
                 AND NOT EXISTS (SELECT 1 FROM orch_tasks ch \
                    WHERE ch.root_id = r.id AND ch.status != 'done')"
            ))?;
            let rows = stmt.query_map([], task_from)?;
            rows.collect::<rusqlite::Result<Vec<_>>>()
        })
        .await
}

/// `waiting` roots with terminally-failed children, plus a failure digest.
pub async fn roots_with_failed_children(store: &Store) -> anyhow::Result<Vec<(OrchTask, String)>> {
    let roots: Vec<OrchTask> = store
        .with_conn(|c| {
            let mut stmt = c.prepare(&format!(
                "SELECT {ORCH_COLS} FROM orch_tasks r WHERE r.root_id IS NULL \
                 AND r.status='waiting' \
                 AND EXISTS (SELECT 1 FROM orch_tasks ch WHERE ch.root_id = r.id \
                    AND ch.status IN ('failed','cancelled'))"
            ))?;
            let rows = stmt.query_map([], task_from)?;
            rows.collect::<rusqlite::Result<Vec<_>>>()
        })
        .await?;
    let mut out = Vec::with_capacity(roots.len());
    for root in roots {
        let children = list_tasks(store, Some(&root.id)).await?;
        let digest = children
            .iter()
            .filter(|t| t.root_id.is_some() && matches!(t.status.as_str(), "failed" | "cancelled"))
            .map(|t| {
                format!(
                    "{} ({}): {}",
                    t.title,
                    t.status,
                    t.error.as_deref().unwrap_or("no error recorded")
                )
            })
            .collect::<Vec<_>>()
            .join("; ");
        out.push((root, digest));
    }
    Ok(out)
}

/// Group a root's children by status, for digests and list rendering.
pub fn children_by_status(tasks: &[OrchTask]) -> BTreeMap<&str, usize> {
    let mut map = BTreeMap::new();
    for t in tasks.iter().filter(|t| t.root_id.is_some()) {
        *map.entry(t.status.as_str()).or_insert(0) += 1;
    }
    map
}

/// Move a task from `expected` to `status` without touching outcome fields.
pub async fn set_status(
    store: &Store,
    id: &str,
    expected: &str,
    status: &str,
) -> anyhow::Result<bool> {
    let (id, expected, status) = (id.to_string(), expected.to_string(), status.to_string());
    store
        .with_conn(move |c| {
            let n = c.execute(
                "UPDATE orch_tasks SET status=?3 WHERE id=?1 AND status=?2",
                params![id, expected, status],
            )?;
            Ok(n > 0)
        })
        .await
}

/// Fail a root from any non-terminal state, recording the error.
pub async fn fail_root(store: &Store, id: &str, error: &str) -> anyhow::Result<bool> {
    let (id, error) = (id.to_string(), error.to_string());
    let now = crate::paths::now_ms();
    store
        .with_conn(move |c| {
            let n = c.execute(
                "UPDATE orch_tasks SET status='failed', error=?2, finished_at=?3 \
                 WHERE id=?1 AND status IN ('decomposing','waiting','judging')",
                params![id, error, now],
            )?;
            Ok(n > 0)
        })
        .await
}

// ---------------------------------------------------------------------------
// Submission + LLM decomposition
// ---------------------------------------------------------------------------

fn emit_changed(cp: &Arc<ControlPlane>, task_id: &str, root_id: Option<String>, status: &str) {
    let _ = cp.send_event(CoreEvent::OrchTaskChanged {
        task_id: task_id.to_string(),
        root_id,
        status: status.to_string(),
    });
}

/// Submit a goal for orchestrated execution. With `decompose`, an LLM plans
/// the subtasks in the background (the root sits in `decomposing` meanwhile);
/// otherwise the goal itself becomes the root's single subtask. `home_session_pk`
/// is the originating chat (if any) worker bubbles post into and the aggregate
/// outcome re-enters over the rail. Returns the root id — the dispatcher loop
/// picks the work up on its next tick.
pub async fn submit(
    cp: &Arc<ControlPlane>,
    project_id: &str,
    goal: &str,
    decompose: bool,
    home_session_pk: Option<&str>,
) -> anyhow::Result<String> {
    if cp.store().get_project(project_id).await?.is_none() {
        anyhow::bail!("unknown project: {project_id}");
    }
    if !decompose {
        return submit_with_plan(
            cp,
            project_id,
            goal,
            single_task_plan(goal),
            home_session_pk,
        )
        .await;
    }
    let root = insert_root(cp.store(), project_id, goal, "decomposing", home_session_pk).await?;
    emit_changed(cp, &root, None, "decomposing");
    let (cp2, root2, project_id, goal) = (
        cp.clone(),
        root.clone(),
        project_id.to_string(),
        goal.to_string(),
    );
    tokio::spawn(async move {
        let roster = crate::harness::native::agents::AgentRegistry::builtin().names();
        let attached = match decompose_goal(cp2.store(), &goal, &roster).await {
            Ok(plan) => attach_plan(&cp2, &root2, &project_id, &plan).await,
            Err(e) => Err(e),
        };
        if let Err(e) = attached {
            // No-op (and no event) when the root was cancelled mid-planning.
            if fail_root(cp2.store(), &root2, &e.to_string())
                .await
                .unwrap_or(false)
            {
                emit_changed(&cp2, &root2, None, "failed");
            }
        }
    });
    Ok(root)
}

/// The trivial one-task plan used when decomposition is off (shared with the
/// CLI's store-only submit path).
pub fn single_task_plan(goal: &str) -> Vec<PlannedTask> {
    vec![PlannedTask {
        title: goal.chars().take(80).collect(),
        body: goal.to_string(),
        agent: "build".into(),
        parents: vec![],
    }]
}

/// Store-only goal submission (no events, no ControlPlane): validate the
/// project, insert a `waiting` root plus the single-task plan. Used by the
/// CLI, whose daemonless process has no event bus; a running daemon host's
/// dispatcher picks the rows up on its next tick. `home_session_pk` is
/// threaded straight to `insert_root`. No in-tree caller passes a home today
/// (a daemonless enqueue has no chat to bind a home session to), but the
/// parameter exists so a future caller with a real chat doesn't need a
/// signature change.
pub async fn queue_goal(
    store: &Store,
    project_id: &str,
    goal: &str,
    home_session_pk: Option<&str>,
) -> anyhow::Result<String> {
    if store.get_project(project_id).await?.is_none() {
        anyhow::bail!("unknown project: {project_id}");
    }
    let root = insert_root(store, project_id, goal, "waiting", home_session_pk).await?;
    insert_children(store, &root, project_id, &single_task_plan(goal)).await?;
    Ok(root)
}

/// Insert a pre-planned decomposition under a fresh `waiting` root. The entry
/// point for non-decomposed submits and for tests that bypass the LLM.
pub async fn submit_with_plan(
    cp: &Arc<ControlPlane>,
    project_id: &str,
    goal: &str,
    plan: Vec<PlannedTask>,
    home_session_pk: Option<&str>,
) -> anyhow::Result<String> {
    let root = insert_root(cp.store(), project_id, goal, "waiting", home_session_pk).await?;
    let ids = insert_children(cp.store(), &root, project_id, &plan).await?;
    emit_changed(cp, &root, None, "waiting");
    for id in &ids {
        emit_changed(cp, id, Some(root.clone()), "todo");
    }
    Ok(root)
}

/// Attach an LLM-produced plan to a `decomposing` root and release it.
/// Children insert and the root flips to `waiting` in ONE transaction guarded
/// by the root still being `decomposing` — a goal cancelled mid-planning gets
/// no children at all (nothing for the dispatcher to orphan-run).
async fn attach_plan(
    cp: &Arc<ControlPlane>,
    root: &str,
    project_id: &str,
    plan: &[PlannedTask],
) -> anyhow::Result<()> {
    let ids: Vec<String> = plan.iter().map(|_| new_task_id()).collect();
    let (root_s, project_id_s, plan_v, ids2, now) = (
        root.to_string(),
        project_id.to_string(),
        plan.to_vec(),
        ids.clone(),
        crate::paths::now_ms(),
    );
    let attached = cp
        .store()
        .with_conn(move |c| {
            let tx = c.transaction()?;
            let released = tx.execute(
                "UPDATE orch_tasks SET status='waiting' \
                 WHERE id=?1 AND status='decomposing'",
                params![&root_s],
            )?;
            if released == 0 {
                // Cancelled (or otherwise moved) while planning: insert nothing.
                return Ok(false);
            }
            for (i, t) in plan_v.iter().enumerate() {
                tx.execute(
                    "INSERT INTO orch_tasks(id,root_id,project_id,title,body,agent,status,created_at) \
                     VALUES (?1,?2,?3,?4,?5,?6,'todo',?7)",
                    params![ids2[i], root_s, project_id_s, t.title, t.body, t.agent, now + i as i64],
                )?;
                for &p in &t.parents {
                    tx.execute(
                        "INSERT INTO orch_task_deps(task_id, dep_id) VALUES (?1, ?2)",
                        params![ids2[i], ids2[p]],
                    )?;
                }
            }
            tx.commit()?;
            Ok(true)
        })
        .await?;
    if !attached {
        anyhow::bail!("root {root} left `decomposing` while planning (cancelled?)");
    }
    emit_changed(cp, root, None, "waiting");
    for id in &ids {
        emit_changed(cp, id, Some(root.to_string()), "todo");
    }
    Ok(())
}

/// One-shot LLM decomposition through the in-process router (no tools).
pub async fn decompose_goal(
    store: &Arc<Store>,
    goal: &str,
    roster: &[String],
) -> anyhow::Result<Vec<PlannedTask>> {
    use crate::llm_router::client::{self, MessageStreamEvent};
    let default_model = client::default_model(store)
        .await
        .ok_or_else(|| anyhow::anyhow!("no default model configured for decomposition"))?;
    let model = crate::harness::native::llm::aux_model(store, "decompose", &default_model).await;
    let ctx = client::UpstreamCtx::new(store.clone());
    let effort_policy = Arc::new(
        crate::llm_router::model_effort::build_utility_effort_policy(store, &model).await?,
    );
    let body = serde_json::json!({
        "model": model,
        "max_tokens": 2048,
        "messages": [{
            "role": "user",
            "content": [{"type": "text", "text": decomposition_prompt(goal, roster)}]
        }],
        "stream": true,
    });
    let crate::llm_router::provenance::RoutedStream { mut events, .. } =
        client::anthropic_messages_stream(&ctx, body, &effort_policy).await?;
    let mut text = String::new();
    while let Some(item) = events.recv().await {
        let ev = item?;
        if let Some(MessageStreamEvent::TextDelta { text: t, .. }) =
            MessageStreamEvent::from_event(&ev)
        {
            text.push_str(&t);
        }
    }
    parse_decomposition(&text, roster)
}

// ---------------------------------------------------------------------------
// Dispatcher
// ---------------------------------------------------------------------------

/// How long one worker/judge session may run before the watcher gives up.
const SESSION_DEADLINE: Duration = Duration::from_secs(2 * 60 * 60);

/// The `max_concurrent_runs` setting (default 3, floor 1).
async fn max_concurrent(store: &Store) -> usize {
    crate::settings::usize_setting(store, "max_concurrent_runs", 3).await
}

/// Spawn the dispatcher loop on the host's runtime (Tauri's setup hook has no
/// ambient tokio context, hence the returned handle — the scheduler pattern).
pub fn spawn_runner(cp: Arc<ControlPlane>) -> tokio::task::JoinHandle<()> {
    tokio::spawn(run_loop(cp))
}

pub async fn run_loop(cp: Arc<ControlPlane>) {
    loop {
        tokio::time::sleep(Duration::from_secs(5)).await;
        tick(&cp).await;
    }
}

/// One dispatcher pass: promote unblocked tasks, start workers up to the
/// concurrency cap, then settle roots (failure digests / judge sessions).
/// Factored out of [`run_loop`] so tests can drive it without sleeping.
pub async fn tick(cp: &Arc<ControlPlane>) {
    let store = cp.store();
    if let Ok(promoted) = promote_ready(store).await {
        for id in &promoted {
            if let Ok(Some(t)) = get_task(store, id).await {
                emit_changed(cp, id, t.root_id.clone(), "ready");
            }
        }
    }
    let cap = max_concurrent(store).await;
    match claim_ready(store, cap).await {
        Ok(claimed) => {
            for t in claimed {
                start_worker(cp, t).await;
            }
        }
        Err(e) => tracing::warn!("orch: claim failed: {e}"),
    }
    if let Ok(failed) = roots_with_failed_children(store).await {
        for (root, digest) in failed {
            if fail_root(store, &root.id, &format!("subtasks failed: {digest}"))
                .await
                .unwrap_or(false)
            {
                emit_changed(cp, &root.id, None, "failed");
                // Park the dead goal's queued siblings (the live-root guards
                // in promote/claim also stop them; this records the outcome).
                match cancel_tree(store, &root.id).await {
                    Ok(n) if n > 0 => {
                        tracing::debug!("orch: cancelled {n} sibling(s) of failed root {}", root.id)
                    }
                    Ok(_) => {}
                    Err(e) => tracing::warn!("orch: sibling cancel failed: {e}"),
                }
            }
        }
    }
    if let Ok(ready) = roots_ready_to_judge(store).await {
        for root in ready {
            start_judge(cp, root).await;
        }
    }
}

/// Start one claimed subtask as its own agent session and watch it to
/// completion in the background.
async fn start_worker(cp: &Arc<ControlPlane>, t: OrchTask) {
    let store = cp.store();
    // Subscribe BEFORE starting so a fast turn can't slip past the watcher.
    let rx = cp.subscribe();
    let prompt = format!("[Orchestrated subtask: {}]\n\n{}", t.title, t.body);
    let home = home_session(store, &t).await.unwrap_or(None);
    let session = match cp
        .start_session_with_prompt(
            &t.project_id,
            crate::harness::TurnPrompt::text(prompt.clone(), prompt),
            "orchestrator",
            &[],
            None,
            None,
            None,
            Some(crate::control::WorkerBinding {
                agent: t.agent.clone(),
                home_session_pk: home,
            }),
        )
        .await
    {
        Ok(s) => s,
        Err(e) => {
            let _ = finish_task(store, &t.id, "running", "failed", None, Some(e.to_string())).await;
            emit_changed(cp, &t.id, t.root_id.clone(), "failed");
            return;
        }
    };
    let _ = set_task_session(store, &t.id, &session.session_pk).await;
    emit_changed(cp, &t.id, t.root_id.clone(), "running");
    let (cp2, task_id, root_id, session_pk) = (
        cp.clone(),
        t.id.clone(),
        t.root_id.clone(),
        session.session_pk.clone(),
    );
    tokio::spawn(async move {
        let outcome = watch_session(cp2.store(), rx, &session_pk).await;
        let (status, result, error) = match outcome {
            Ok(()) => {
                let report = crate::scheduler::final_assistant_text(cp2.store(), &session_pk).await;
                ("done", report, None)
            }
            Err(e) => ("failed", None, Some(e)),
        };
        match finish_task(cp2.store(), &task_id, "running", status, result, error).await {
            Ok(true) => emit_changed(&cp2, &task_id, root_id, status),
            _ => tracing::debug!("orch: task {task_id} outcome discarded (cancelled?)"),
        }
    });
}

/// Start the judge session for a root whose children are all done.
async fn start_judge(cp: &Arc<ControlPlane>, root: OrchTask) {
    let store = cp.store();
    if !set_status(store, &root.id, "waiting", "judging")
        .await
        .unwrap_or(false)
    {
        return; // cancelled or already picked up
    }
    emit_changed(cp, &root.id, None, "judging");
    let children = match list_tasks(store, Some(&root.id)).await {
        Ok(c) => c,
        Err(e) => {
            let _ = fail_root(store, &root.id, &e.to_string()).await;
            emit_changed(cp, &root.id, None, "failed");
            return;
        }
    };
    let rx = cp.subscribe();
    let session = match cp
        .start_session(
            &root.project_id,
            &judge_prompt(&root, &children),
            "orchestrator",
            &[],
        )
        .await
    {
        Ok(s) => s,
        Err(e) => {
            let _ = fail_root(store, &root.id, &e.to_string()).await;
            emit_changed(cp, &root.id, None, "failed");
            return;
        }
    };
    let _ = set_task_session(store, &root.id, &session.session_pk).await;
    let (cp2, root_id, session_pk) = (cp.clone(), root.id.clone(), session.session_pk.clone());
    tokio::spawn(async move {
        let outcome = watch_session(cp2.store(), rx, &session_pk).await;
        let changed = match outcome {
            Ok(()) => {
                let verdict =
                    crate::scheduler::final_assistant_text(cp2.store(), &session_pk).await;
                finish_task(cp2.store(), &root_id, "judging", "done", verdict, None).await
            }
            Err(e) => finish_task(cp2.store(), &root_id, "judging", "failed", None, Some(e)).await,
        };
        if changed.unwrap_or(false) {
            let status = get_task(cp2.store(), &root_id)
                .await
                .ok()
                .flatten()
                .map(|t| t.status)
                .unwrap_or_else(|| "done".into());
            emit_changed(&cp2, &root_id, None, &status);
        }
    });
}

fn judge_prompt(root: &OrchTask, children: &[OrchTask]) -> String {
    let mut s = format!(
        "You are judging an orchestrated goal that was decomposed into subtasks, \
         each run by its own agent session.\n\nGoal:\n{}\n\nSubtask outcomes:\n",
        root.body
    );
    for c in children.iter().filter(|c| c.root_id.is_some()) {
        let report: String = c
            .result
            .as_deref()
            .unwrap_or("(no report)")
            .chars()
            .take(2000)
            .collect();
        s.push_str(&format!("- {}: {report}\n", c.title));
    }
    s.push_str(
        "\nVerify the goal is satisfied by these outcomes; fix small gaps yourself \
         where possible. Reply with one final report of the overall outcome.",
    );
    s
}

/// Wait for the session's terminal event on the broadcast bus (the
/// scheduler's watcher shape, 2h deadline). A lagged receiver may have missed
/// the terminal event entirely, so on `Lagged` the session row is consulted:
/// a session no longer `Running` finished while we weren't looking.
async fn watch_session(
    store: &Store,
    mut rx: broadcast::Receiver<CoreEvent>,
    session_pk: &str,
) -> Result<(), String> {
    let deadline = tokio::time::Instant::now() + SESSION_DEADLINE;
    loop {
        match tokio::time::timeout_at(deadline, rx.recv()).await {
            Ok(Ok(CoreEvent::Result { session_pk: pk })) if pk == session_pk => return Ok(()),
            Ok(Ok(CoreEvent::Error {
                session_pk: pk,
                message,
            })) if pk == session_pk => return Err(message),
            Ok(Ok(_)) => continue,
            Ok(Err(broadcast::error::RecvError::Lagged(_))) => {
                if let Ok(Some(session)) = store.get_session(session_pk).await {
                    if session.status != crate::domain::SessionStatus::Running {
                        return Ok(()); // terminal event was among the drops
                    }
                }
                continue;
            }
            Ok(Err(_)) => return Err("event bus closed".into()),
            Err(_) => return Err("timed out after 2h".into()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn roster() -> Vec<String> {
        vec!["build".into(), "explore".into()]
    }

    #[test]
    fn parse_happy_path_with_fences_and_prose() {
        let raw = "Here is the plan:\n```json\n{\"tasks\": [\
            {\"title\": \"a\", \"body\": \"do a\", \"agent\": \"explore\", \"parents\": []},\
            {\"title\": \"b\", \"body\": \"do b\", \"agent\": \"build\", \"parents\": [0]}\
        ]}\n```\nGood luck!";
        let plan = parse_decomposition(raw, &roster()).unwrap();
        assert_eq!(plan.len(), 2);
        assert_eq!(plan[0].agent, "explore");
        assert_eq!(plan[1].parents, vec![0]);
    }

    #[test]
    fn parse_defaults_unknown_agent_to_build() {
        let raw = "{\"tasks\": [\
            {\"title\": \"a\", \"body\": \"x\", \"agent\": \"wizard\"},\
            {\"title\": \"b\", \"body\": \"y\"}\
        ]}";
        let plan = parse_decomposition(raw, &roster()).unwrap();
        assert_eq!(plan[0].agent, "build");
        assert_eq!(plan[1].agent, "build");
    }

    #[test]
    fn parse_rejects_bad_shapes() {
        let r = roster();
        // Not JSON at all.
        assert!(parse_decomposition("no json here", &r).is_err());
        // Too few tasks.
        let one = "{\"tasks\": [{\"title\": \"a\", \"body\": \"x\"}]}";
        assert!(parse_decomposition(one, &r)
            .unwrap_err()
            .to_string()
            .contains("2-6"));
        // Out-of-range parent.
        let oor = "{\"tasks\": [\
            {\"title\": \"a\", \"body\": \"x\", \"parents\": [5]},\
            {\"title\": \"b\", \"body\": \"y\"}]}";
        assert!(parse_decomposition(oor, &r)
            .unwrap_err()
            .to_string()
            .contains("out-of-range"));
        // Cycle.
        let cyc = "{\"tasks\": [\
            {\"title\": \"a\", \"body\": \"x\", \"parents\": [1]},\
            {\"title\": \"b\", \"body\": \"y\", \"parents\": [0]}]}";
        assert!(parse_decomposition(cyc, &r)
            .unwrap_err()
            .to_string()
            .contains("cycle"));
        // Missing body.
        let nb = "{\"tasks\": [\
            {\"title\": \"a\", \"body\": \"\"},\
            {\"title\": \"b\", \"body\": \"y\"}]}";
        assert!(parse_decomposition(nb, &r).is_err());
    }

    #[test]
    fn prompt_carries_goal_and_roster() {
        let p = decomposition_prompt("ship the feature", &roster());
        assert!(p.contains("ship the feature"));
        assert!(p.contains("build, explore"));
        assert!(p.contains("2-6 subtasks"));
    }

    async fn store() -> Store {
        let tmp = tempfile::NamedTempFile::new().unwrap();
        let store = Store::open(tmp.path()).await.unwrap();
        // Keep the tempfile alive for the store's lifetime.
        std::mem::forget(tmp);
        store
    }

    fn plan_ab_c() -> Vec<PlannedTask> {
        vec![
            PlannedTask {
                title: "a".into(),
                body: "do a".into(),
                agent: "build".into(),
                parents: vec![],
            },
            PlannedTask {
                title: "b".into(),
                body: "do b".into(),
                agent: "build".into(),
                parents: vec![],
            },
            PlannedTask {
                title: "c".into(),
                body: "do c after a+b".into(),
                agent: "build".into(),
                parents: vec![0, 1],
            },
        ]
    }

    #[tokio::test]
    async fn promotion_follows_dependencies() {
        let s = store().await;
        let root = insert_root(&s, "p1", "the goal", "waiting", None)
            .await
            .unwrap();
        let ids = insert_children(&s, &root, "p1", &plan_ab_c())
            .await
            .unwrap();

        // First pass: only the dep-free children promote.
        let promoted = promote_ready(&s).await.unwrap();
        assert_eq!(promoted.len(), 2);
        assert!(!promoted.contains(&ids[2]));

        // Finish a — c still gated by b.
        let claimed = claim_ready(&s, 10).await.unwrap();
        assert_eq!(claimed.len(), 2);
        assert!(
            finish_task(&s, &ids[0], "running", "done", Some("a done".into()), None)
                .await
                .unwrap()
        );
        assert!(promote_ready(&s).await.unwrap().is_empty());

        // Finish b — now c promotes.
        finish_task(&s, &ids[1], "running", "done", None, None)
            .await
            .unwrap();
        assert_eq!(promote_ready(&s).await.unwrap(), vec![ids[2].clone()]);
    }

    #[tokio::test]
    async fn claim_respects_concurrency_cap() {
        let s = store().await;
        let root = insert_root(&s, "p1", "goal", "waiting", None)
            .await
            .unwrap();
        let plan: Vec<PlannedTask> = (0..4)
            .map(|i| PlannedTask {
                title: format!("t{i}"),
                body: "x".into(),
                agent: "build".into(),
                parents: vec![],
            })
            .collect();
        insert_children(&s, &root, "p1", &plan).await.unwrap();
        promote_ready(&s).await.unwrap();

        let first = claim_ready(&s, 2).await.unwrap();
        assert_eq!(first.len(), 2, "cap limits claims");
        let second = claim_ready(&s, 2).await.unwrap();
        assert!(second.is_empty(), "running tasks occupy the cap");
        finish_task(&s, &first[0].id, "running", "done", None, None)
            .await
            .unwrap();
        assert_eq!(claim_ready(&s, 2).await.unwrap().len(), 1);
    }

    #[tokio::test]
    async fn judge_readiness_and_failure_digest() {
        let s = store().await;
        let root = insert_root(&s, "p1", "goal", "waiting", None)
            .await
            .unwrap();
        let ids = insert_children(&s, &root, "p1", &plan_ab_c())
            .await
            .unwrap();
        assert!(roots_ready_to_judge(&s).await.unwrap().is_empty());

        promote_ready(&s).await.unwrap();
        claim_ready(&s, 10).await.unwrap();
        finish_task(&s, &ids[0], "running", "done", None, None)
            .await
            .unwrap();
        finish_task(&s, &ids[1], "running", "failed", None, Some("boom".into()))
            .await
            .unwrap();

        let failed = roots_with_failed_children(&s).await.unwrap();
        assert_eq!(failed.len(), 1);
        assert_eq!(failed[0].0.id, root);
        assert!(failed[0].1.contains("boom"), "{}", failed[0].1);

        // A fully-done tree instead becomes judge-ready.
        let root2 = insert_root(&s, "p1", "goal2", "waiting", None)
            .await
            .unwrap();
        let ids2 = insert_children(
            &s,
            &root2,
            "p1",
            &[
                PlannedTask {
                    title: "x".into(),
                    body: "x".into(),
                    agent: "build".into(),
                    parents: vec![],
                },
                PlannedTask {
                    title: "y".into(),
                    body: "y".into(),
                    agent: "build".into(),
                    parents: vec![],
                },
            ],
        )
        .await
        .unwrap();
        promote_ready(&s).await.unwrap();
        claim_ready(&s, 10).await.unwrap();
        for id in &ids2 {
            finish_task(&s, id, "running", "done", None, None)
                .await
                .unwrap();
        }
        let ready = roots_ready_to_judge(&s).await.unwrap();
        assert_eq!(ready.len(), 1);
        assert_eq!(ready[0].id, root2);
    }

    #[tokio::test]
    async fn cancel_tree_spares_finished_children_and_guards_races() {
        let s = store().await;
        let root = insert_root(&s, "p1", "goal", "waiting", None)
            .await
            .unwrap();
        let ids = insert_children(&s, &root, "p1", &plan_ab_c())
            .await
            .unwrap();
        promote_ready(&s).await.unwrap();
        claim_ready(&s, 10).await.unwrap();
        finish_task(&s, &ids[0], "running", "done", Some("kept".into()), None)
            .await
            .unwrap();

        let n = cancel_tree(&s, &root).await.unwrap();
        assert_eq!(n, 3, "root + b (running) + c (todo)");
        let a = get_task(&s, &ids[0]).await.unwrap().unwrap();
        assert_eq!(a.status, "done", "finished children are spared");

        // The still-running b's watcher outcome loses the race.
        assert!(
            !finish_task(&s, &ids[1], "running", "done", Some("late".into()), None)
                .await
                .unwrap()
        );
        let b = get_task(&s, &ids[1]).await.unwrap().unwrap();
        assert_eq!(b.status, "cancelled");
    }

    // -- dispatcher integration ---------------------------------------------

    use crate::harness::{Harness, HarnessFactory, HarnessSession, SessionCtx, TurnPrompt};
    use async_trait::async_trait;

    /// A harness whose sessions write one assistant text row and finish.
    struct EchoHarness;
    struct EchoSession {
        store: Arc<Store>,
        session_pk: String,
    }

    #[async_trait]
    impl HarnessSession for EchoSession {
        async fn send_prompt(&self, prompt: TurnPrompt) -> anyhow::Result<()> {
            let first_line = prompt.display.lines().next().unwrap_or("").to_string();
            self.store
                .insert_message(crate::domain::NewMessage {
                    session_pk: self.session_pk.clone(),
                    role: "assistant".into(),
                    block_type: "text".into(),
                    payload: serde_json::json!({
                        "text": format!("worked on: {first_line}")
                    }),
                    tool_call_id: None,
                    status: None,
                    tool_kind: None,
                    speaker: None,
                })
                .await?;
            Ok(())
        }
        async fn cancel(&self) -> anyhow::Result<()> {
            Ok(())
        }
        async fn end(&self) -> anyhow::Result<()> {
            Ok(())
        }
        fn agent_session_id(&self) -> Option<String> {
            Some(self.session_pk.clone())
        }
    }

    #[async_trait]
    impl Harness for EchoHarness {
        async fn start_session(&self, ctx: SessionCtx) -> anyhow::Result<Box<dyn HarnessSession>> {
            // Mirrors a real harness refusing to start in a work_dir that
            // doesn't exist on disk — the single-slot registry no longer
            // lets a test force a start failure via an unresolvable harness
            // id, so `worker_start_failure_fails_task_then_root` triggers
            // this by pointing a project at a nonexistent directory instead.
            if !ctx.work_dir.exists() {
                anyhow::bail!("work_dir does not exist: {}", ctx.work_dir.display());
            }
            Ok(Box::new(EchoSession {
                store: ctx.store.clone(),
                session_pk: ctx.session_pk.clone(),
            }))
        }
    }

    struct EchoHarnessFactory;
    impl HarnessFactory for EchoHarnessFactory {
        fn create(&self) -> anyhow::Result<Arc<dyn Harness>> {
            Ok(Arc::new(EchoHarness))
        }
    }

    /// A ControlPlane wired to the echo harness plus a project rooted at a
    /// fresh git repo (worktree creation needs a HEAD commit).
    async fn cp_with_project() -> (Arc<ControlPlane>, tempfile::TempDir) {
        let repo = tempfile::tempdir().unwrap();
        {
            let r = git2::Repository::init(repo.path()).unwrap();
            let sig = git2::Signature::now("t", "t@t").unwrap();
            let tree_id = r.index().unwrap().write_tree().unwrap();
            let tree = r.find_tree(tree_id).unwrap();
            r.commit(Some("HEAD"), &sig, &sig, "init", &tree, &[])
                .unwrap();
        }
        let tmp = tempfile::NamedTempFile::new().unwrap();
        let store = Store::open(tmp.path()).await.unwrap();
        std::mem::forget(tmp);
        store
            .insert_project(crate::domain::Project {
                project_id: "p1".into(),
                name: "p1".into(),
                workdir: repo.path().to_string_lossy().into_owned(),
                source: None,
                model: None,
                effort: None,
                perm_mode: crate::domain::PermMode::Default,
                created_at: Some(0),
                is_git: true,
            })
            .await
            .unwrap();
        let mut regs = crate::plugins::Registries::new();
        regs.harness = Arc::new(EchoHarnessFactory);
        let cp = ControlPlane::new(store, regs).await;
        (cp, repo)
    }

    async fn drive_until(
        cp: &Arc<ControlPlane>,
        root: &str,
        target: &str,
        max_ms: u64,
    ) -> OrchTask {
        let deadline = tokio::time::Instant::now() + Duration::from_millis(max_ms);
        loop {
            tick(cp).await;
            let t = get_task(cp.store(), root).await.unwrap().unwrap();
            if t.status == target {
                return t;
            }
            assert!(
                tokio::time::Instant::now() < deadline,
                "root stuck in `{}` waiting for `{target}`",
                t.status
            );
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    }

    #[tokio::test]
    async fn dispatcher_runs_plan_to_root_judgment() {
        let (cp, _repo) = cp_with_project().await;
        let mut rx = cp.subscribe();
        let root = submit_with_plan(
            &cp,
            "p1",
            "ship the widget",
            vec![
                PlannedTask {
                    title: "research".into(),
                    body: "find prior art".into(),
                    agent: "explore".into(),
                    parents: vec![],
                },
                PlannedTask {
                    title: "implement".into(),
                    body: "build the widget".into(),
                    agent: "build".into(),
                    parents: vec![0],
                },
            ],
            None,
        )
        .await
        .unwrap();

        let done = drive_until(&cp, &root, "done", 10_000).await;
        // The judge session's report became the root's result.
        assert!(
            done.result.as_deref().unwrap_or("").contains("worked on:"),
            "{:?}",
            done.result
        );

        // Children completed in dependency order with captured reports.
        let children: Vec<OrchTask> = list_tasks(cp.store(), Some(&root))
            .await
            .unwrap()
            .into_iter()
            .filter(|t| t.root_id.is_some())
            .collect();
        assert_eq!(children.len(), 2);
        assert!(children.iter().all(|c| c.status == "done"));
        assert!(children
            .iter()
            .all(|c| c.result.as_deref().unwrap_or("").contains("worked on:")));
        assert!(children.iter().all(|c| c.session_pk.is_some()));

        // The event stream saw the key transitions.
        let mut seen = Vec::new();
        while let Ok(ev) = rx.try_recv() {
            if let CoreEvent::OrchTaskChanged { status, .. } = ev {
                seen.push(status);
            }
        }
        for expected in ["waiting", "todo", "ready", "running", "judging", "done"] {
            assert!(
                seen.iter().any(|s| s == expected),
                "missing `{expected}` in {seen:?}"
            );
        }
    }

    #[tokio::test]
    async fn worker_start_failure_fails_task_then_root() {
        let (cp, _repo) = cp_with_project().await;
        // A project rooted at a nonexistent work_dir: EchoHarness::start_session
        // refuses to start (see its doc), so start_session errors.
        cp.store()
            .insert_project(crate::domain::Project {
                project_id: "p2".into(),
                name: "p2".into(),
                workdir: "C:/nonexistent-dir-for-orch-test".into(),
                source: None,
                model: None,
                effort: None,
                perm_mode: crate::domain::PermMode::Default,
                created_at: Some(0),
                is_git: false,
            })
            .await
            .unwrap();
        let root = submit_with_plan(
            &cp,
            "p2",
            "doomed goal",
            vec![PlannedTask {
                title: "will not start".into(),
                body: "x".into(),
                agent: "build".into(),
                parents: vec![],
            }],
            None,
        )
        .await
        .unwrap();

        let failed = drive_until(&cp, &root, "failed", 10_000).await;
        assert!(
            failed
                .error
                .as_deref()
                .unwrap_or("")
                .contains("subtasks failed"),
            "{:?}",
            failed.error
        );
    }

    #[tokio::test]
    async fn submit_without_decompose_queues_a_single_child() {
        let (cp, _repo) = cp_with_project().await;
        let root = submit(&cp, "p1", "just do it", false, None).await.unwrap();
        let tasks = list_tasks(cp.store(), Some(&root)).await.unwrap();
        let (roots, children): (Vec<_>, Vec<_>) =
            tasks.into_iter().partition(|t| t.root_id.is_none());
        assert_eq!(roots.len(), 1);
        assert_eq!(roots[0].status, "waiting");
        assert_eq!(children.len(), 1);
        assert_eq!(children[0].status, "todo");
        assert_eq!(children[0].body, "just do it");
        // Unknown projects are rejected up front.
        assert!(submit(&cp, "nope", "x", false, None).await.is_err());
    }

    #[tokio::test]
    async fn worker_session_runs_its_assigned_agent_and_binds_the_home_chat() {
        let (cp, _repo) = cp_with_project().await;
        let project_id = cp.store().list_projects().await.unwrap()[0]
            .project_id
            .clone();
        // Two-task plan with distinct assignees.
        let root = submit_with_plan(
            &cp,
            &project_id,
            "goal",
            vec![PlannedTask {
                title: "a".into(),
                body: "do a".into(),
                agent: "plan".into(),
                parents: vec![],
            }],
            Some("home-42"),
        )
        .await
        .unwrap();
        tick(&cp).await; // promote + claim + start_worker
                         // The claimed child now has a worker session with the right shape.
        let child = list_tasks(cp.store(), Some(&root))
            .await
            .unwrap()
            .into_iter()
            .find(|t| t.root_id.is_some())
            .unwrap();
        let s = cp
            .store()
            .get_session(child.session_pk.as_deref().unwrap())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(s.kind, crate::domain::SessionKind::Worker);
        assert_eq!(s.agent.as_deref(), Some("plan"));
        assert_eq!(s.speaker.as_deref(), Some("plan"));
        assert_eq!(s.parent_session_pk.as_deref(), Some("home-42"));
    }

    #[tokio::test]
    async fn dead_roots_park_their_remaining_children() {
        let s = store().await;
        let root = insert_root(&s, "p1", "goal", "waiting", None)
            .await
            .unwrap();
        insert_children(&s, &root, "p1", &plan_ab_c())
            .await
            .unwrap();
        // Root dies before anything was promoted.
        assert!(fail_root(&s, &root, "boom").await.unwrap());
        assert!(promote_ready(&s).await.unwrap().is_empty(), "no promotion");
        assert!(claim_ready(&s, 10).await.unwrap().is_empty(), "no claims");
    }

    #[tokio::test]
    async fn retry_child_revives_failed_root_and_cancelled_siblings() {
        let s = store().await;
        let root = insert_root(&s, "p1", "goal", "waiting", None)
            .await
            .unwrap();
        let ids = insert_children(&s, &root, "p1", &plan_ab_c())
            .await
            .unwrap();
        promote_ready(&s).await.unwrap();
        claim_ready(&s, 10).await.unwrap();
        finish_task(&s, &ids[0], "running", "failed", None, Some("x".into()))
            .await
            .unwrap();
        // The dispatcher's failure pass: root fails, siblings park.
        assert!(fail_root(&s, &root, "subtasks failed: x").await.unwrap());
        cancel_tree(&s, &root).await.unwrap();

        assert!(retry_task(&s, &ids[0]).await.unwrap());
        let root_row = get_task(&s, &root).await.unwrap().unwrap();
        assert_eq!(root_row.status, "waiting", "root revived");
        assert!(root_row.error.is_none());
        for id in &ids {
            assert_eq!(get_task(&s, id).await.unwrap().unwrap().status, "todo");
        }
        // The revived tree promotes again.
        assert_eq!(promote_ready(&s).await.unwrap().len(), 2);
    }

    #[tokio::test]
    async fn retry_root_requeues_failed_children_but_not_childless_roots() {
        let s = store().await;
        // Childless failed root (decomposition failure): nothing to re-run.
        let bare = insert_root(&s, "p1", "goal", "waiting", None)
            .await
            .unwrap();
        fail_root(&s, &bare, "no plan").await.unwrap();
        assert!(!retry_task(&s, &bare).await.unwrap());

        // Root with failed children: everything re-queues.
        let root = insert_root(&s, "p1", "goal2", "waiting", None)
            .await
            .unwrap();
        let ids = insert_children(&s, &root, "p1", &plan_ab_c())
            .await
            .unwrap();
        promote_ready(&s).await.unwrap();
        claim_ready(&s, 10).await.unwrap();
        finish_task(&s, &ids[0], "running", "failed", None, Some("x".into()))
            .await
            .unwrap();
        fail_root(&s, &root, "subtasks failed").await.unwrap();
        cancel_tree(&s, &root).await.unwrap();
        assert!(retry_task(&s, &root).await.unwrap());
        assert_eq!(
            get_task(&s, &root).await.unwrap().unwrap().status,
            "waiting"
        );
        assert_eq!(get_task(&s, &ids[0]).await.unwrap().unwrap().status, "todo");
    }

    #[tokio::test]
    async fn attach_plan_inserts_nothing_for_a_cancelled_root() {
        let (cp, _repo) = cp_with_project().await;
        let root = insert_root(cp.store(), "p1", "goal", "decomposing", None)
            .await
            .unwrap();
        // Cancelled while the (simulated) LLM planning was in flight.
        cancel_tree(cp.store(), &root).await.unwrap();
        let err = attach_plan(&cp, &root, "p1", &plan_ab_c())
            .await
            .unwrap_err()
            .to_string();
        assert!(err.contains("cancelled"), "{err}");
        let tasks = list_tasks(cp.store(), Some(&root)).await.unwrap();
        assert_eq!(tasks.len(), 1, "no orphaned children were inserted");
        assert_eq!(tasks[0].status, "cancelled");
    }

    #[tokio::test]
    async fn retry_requeues_only_failed_tasks() {
        let s = store().await;
        let root = insert_root(&s, "p1", "goal", "waiting", None)
            .await
            .unwrap();
        let ids = insert_children(&s, &root, "p1", &plan_ab_c())
            .await
            .unwrap();
        promote_ready(&s).await.unwrap();
        claim_ready(&s, 10).await.unwrap();
        finish_task(&s, &ids[0], "running", "failed", None, Some("x".into()))
            .await
            .unwrap();

        assert!(retry_task(&s, &ids[0]).await.unwrap());
        let a = get_task(&s, &ids[0]).await.unwrap().unwrap();
        assert_eq!(a.status, "todo");
        assert!(a.error.is_none());
        // Non-failed tasks refuse.
        assert!(!retry_task(&s, &ids[1]).await.unwrap());
    }
}
