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

use crate::store::Store;
use rusqlite::{params, OptionalExtension};
use std::collections::BTreeMap;

/// One row of the orchestrated task graph.
#[derive(Debug, Clone, PartialEq)]
pub struct OrchTask {
    pub id: String,
    /// `None` for a root (goal) task.
    pub root_id: Option<String>,
    pub project_id: String,
    pub title: String,
    pub body: String,
    /// Advisory: recorded from the decomposer; worker sessions currently run
    /// the project's default agent (start_session has no agent selection yet).
    pub agent: String,
    pub status: String,
    pub session_pk: Option<String>,
    pub result: Option<String>,
    pub error: Option<String>,
    pub created_at: i64,
    pub finished_at: Option<i64>,
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
    loop {
        let Some(next) = (0..n).find(|&i| !done[i] && unmet[i] == 0) else {
            break;
        };
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
    "id,root_id,project_id,title,body,agent,status,session_pk,result,error,created_at,finished_at";

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
) -> anyhow::Result<String> {
    let id = new_task_id();
    let title: String = goal.chars().take(80).collect();
    let (id2, project_id, goal, status, now) = (
        id.clone(),
        project_id.to_string(),
        goal.to_string(),
        status.to_string(),
        crate::paths::now_ms(),
    );
    store
        .with_conn(move |c| {
            c.execute(
                "INSERT INTO orch_tasks(id,root_id,project_id,title,body,agent,status,created_at) \
                 VALUES (?1,NULL,?2,?3,?4,'',?5,?6)",
                params![id2, project_id, title, goal, status, now],
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

/// Flip `todo` children whose dependencies are all `done` to `ready`.
/// Returns the promoted ids.
pub async fn promote_ready(store: &Store) -> anyhow::Result<Vec<String>> {
    store
        .with_conn(|c| {
            let tx = c.transaction()?;
            let promoted: Vec<String> = {
                let mut stmt = tx.prepare(
                    "SELECT id FROM orch_tasks t WHERE t.status='todo' AND NOT EXISTS (\
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
/// first. Returns the claimed rows.
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
                    "SELECT {ORCH_COLS} FROM orch_tasks WHERE status='ready' \
                     ORDER BY created_at LIMIT ?1"
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

/// Re-queue a `failed` task as `todo`, clearing its previous outcome.
pub async fn retry_task(store: &Store, id: &str) -> anyhow::Result<bool> {
    let id = id.to_string();
    store
        .with_conn(move |c| {
            let n = c.execute(
                "UPDATE orch_tasks SET status='todo', error=NULL, result=NULL, \
                 session_pk=NULL, finished_at=NULL WHERE id=?1 AND status='failed'",
                params![id],
            )?;
            Ok(n > 0)
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
        .map_err(Into::into)
}

/// `waiting` roots with terminally-failed children, plus a failure digest.
pub async fn roots_with_failed_children(
    store: &Store,
) -> anyhow::Result<Vec<(OrchTask, String)>> {
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
        let root = insert_root(&s, "p1", "the goal", "waiting").await.unwrap();
        let ids = insert_children(&s, &root, "p1", &plan_ab_c()).await.unwrap();

        // First pass: only the dep-free children promote.
        let promoted = promote_ready(&s).await.unwrap();
        assert_eq!(promoted.len(), 2);
        assert!(!promoted.contains(&ids[2]));

        // Finish a — c still gated by b.
        let claimed = claim_ready(&s, 10).await.unwrap();
        assert_eq!(claimed.len(), 2);
        assert!(finish_task(&s, &ids[0], "running", "done", Some("a done".into()), None)
            .await
            .unwrap());
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
        let root = insert_root(&s, "p1", "goal", "waiting").await.unwrap();
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
        let root = insert_root(&s, "p1", "goal", "waiting").await.unwrap();
        let ids = insert_children(&s, &root, "p1", &plan_ab_c()).await.unwrap();
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
        let root2 = insert_root(&s, "p1", "goal2", "waiting").await.unwrap();
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
        let root = insert_root(&s, "p1", "goal", "waiting").await.unwrap();
        let ids = insert_children(&s, &root, "p1", &plan_ab_c()).await.unwrap();
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

    #[tokio::test]
    async fn retry_requeues_only_failed_tasks() {
        let s = store().await;
        let root = insert_root(&s, "p1", "goal", "waiting").await.unwrap();
        let ids = insert_children(&s, &root, "p1", &plan_ab_c()).await.unwrap();
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
