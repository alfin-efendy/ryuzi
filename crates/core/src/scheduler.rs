//! Scheduler domain: persisted jobs with cron schedules that really run —
//! a background loop starts an agent session with the job's prompt when a
//! schedule fires, and the run row closes when that session's turn completes.

use crate::control::ControlPlane;
use crate::domain::CoreEvent;
use crate::store::Store;
use chrono::{DateTime, Local, TimeZone};
use croner::Cron;
use rusqlite::{params, OptionalExtension};
use std::sync::Arc;
use std::time::Duration;

#[derive(Debug, Clone, PartialEq)]
pub struct JobRow {
    pub id: String,
    pub name: String,
    pub cron: String,
    /// natural | cron
    pub mode: String,
    pub natural_text: String,
    pub project_id: String,
    pub branch: String,
    pub agent: String,
    pub gateway: String,
    pub enabled: bool,
    pub prompt: String,
    pub notify_success: bool,
    pub notify_fail: bool,
    /// Optional wake-gate command run before the agent wakes: empty stdout,
    /// non-zero exit, or timeout skips the fire; stdout is otherwise appended
    /// to the prompt as context.
    pub pre_check: String,
}

#[derive(Debug, Clone, PartialEq)]
pub struct RunRow {
    pub id: String,
    pub job_id: String,
    /// running | success | failed
    pub status: String,
    pub started_at: i64,
    pub finished_at: Option<i64>,
    pub session_pk: Option<String>,
    pub error: Option<String>,
    pub add_lines: Option<i64>,
    pub del_lines: Option<i64>,
    pub note: Option<String>,
    pub log: Option<String>,
}

// ---------------------------------------------------------------------------
// Cron / natural language
// ---------------------------------------------------------------------------

/// Next occurrence of `cron_expr` strictly after `after` (epoch ms), in the
/// local timezone. None when the expression is invalid.
pub fn next_run_after(cron_expr: &str, after_ms: i64) -> Option<i64> {
    let cron = Cron::new(cron_expr).parse().ok()?;
    let after: DateTime<Local> = Local.timestamp_millis_opt(after_ms).single()?;
    let next = cron.find_next_occurrence(&after, false).ok()?;
    Some(next.timestamp_millis())
}

/// Rule-based English → cron for the patterns the UI offers. Returns None for
/// anything it can't parse confidently (the UI then asks for cron mode).
pub fn natural_to_cron(text: &str) -> Option<String> {
    let t = text.trim().to_lowercase();
    let t = t.strip_prefix("every ").unwrap_or(&t).trim().to_string();

    const DAYS: [(&str, u8); 7] = [
        ("sunday", 0),
        ("monday", 1),
        ("tuesday", 2),
        ("wednesday", 3),
        ("thursday", 4),
        ("friday", 5),
        ("saturday", 6),
    ];

    // "N minutes" / "minute"
    if t == "minute" {
        return Some("* * * * *".into());
    }
    if let Some(rest) = t
        .strip_suffix(" minutes")
        .or_else(|| t.strip_suffix(" mins"))
    {
        let n: u32 = rest.trim().parse().ok()?;
        if (1..60).contains(&n) {
            return Some(format!("*/{n} * * * *"));
        }
        return None;
    }
    // "N hours" / "hour"
    if t == "hour" {
        return Some("0 * * * *".into());
    }
    if let Some(rest) = t.strip_suffix(" hours") {
        let n: u32 = rest.trim().parse().ok()?;
        if (1..24).contains(&n) {
            return Some(format!("0 */{n} * * *"));
        }
        return None;
    }

    // "<scope> at <time>" where scope ∈ day | weekday name | weekdays
    let (scope, time) = match t.split_once(" at ") {
        Some((s, time)) => (s.trim(), time.trim()),
        None => return None,
    };
    let (hour, minute) = parse_time(time)?;
    if scope == "day" {
        return Some(format!("{minute} {hour} * * *"));
    }
    if scope == "weekday" || scope == "weekdays" {
        return Some(format!("{minute} {hour} * * 1-5"));
    }
    for (name, num) in DAYS {
        if scope == name || scope == name.trim_end_matches("day") {
            return Some(format!("{minute} {hour} * * {num}"));
        }
    }
    None
}

/// "2am", "9pm", "14:30", "9:15am", "12am" (midnight), "12pm" (noon).
fn parse_time(t: &str) -> Option<(u32, u32)> {
    let t = t.trim();
    let (body, pm) = if let Some(b) = t.strip_suffix("pm") {
        (b.trim(), Some(true))
    } else if let Some(b) = t.strip_suffix("am") {
        (b.trim(), Some(false))
    } else {
        (t, None)
    };
    let (h, m) = match body.split_once(':') {
        Some((h, m)) => (h.trim().parse::<u32>().ok()?, m.trim().parse::<u32>().ok()?),
        None => (body.trim().parse::<u32>().ok()?, 0),
    };
    if m >= 60 {
        return None;
    }
    let hour = match pm {
        Some(true) => {
            if h == 12 {
                12
            } else if h < 12 {
                h + 12
            } else {
                return None;
            }
        }
        Some(false) => {
            if h == 12 {
                0
            } else if h < 12 {
                h
            } else {
                return None;
            }
        }
        None => {
            if h < 24 {
                h
            } else {
                return None;
            }
        }
    };
    Some((hour, m))
}

// ---------------------------------------------------------------------------
// Persistence
// ---------------------------------------------------------------------------

const JOB_COLS: &str =
    "id,name,cron,mode,natural_text,project_id,branch,agent,gateway,enabled,prompt,notify_success,notify_fail,pre_check";

fn job_from(r: &rusqlite::Row) -> rusqlite::Result<JobRow> {
    Ok(JobRow {
        id: r.get(0)?,
        name: r.get(1)?,
        cron: r.get(2)?,
        mode: r.get(3)?,
        natural_text: r.get(4)?,
        project_id: r.get(5)?,
        branch: r.get(6)?,
        agent: r.get(7)?,
        gateway: r.get(8)?,
        enabled: r.get::<_, i64>(9)? != 0,
        prompt: r.get(10)?,
        notify_success: r.get::<_, i64>(11)? != 0,
        notify_fail: r.get::<_, i64>(12)? != 0,
        pre_check: r.get(13)?,
    })
}

pub async fn list_jobs(store: &Store) -> anyhow::Result<Vec<JobRow>> {
    store
        .with_conn(|c| {
            let mut stmt = c.prepare(&format!(
                "SELECT {JOB_COLS} FROM jobs ORDER BY created_at DESC"
            ))?;
            let rows = stmt
                .query_map([], job_from)?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            Ok(rows)
        })
        .await
}

pub async fn get_job(store: &Store, id: &str) -> anyhow::Result<Option<JobRow>> {
    let id = id.to_string();
    store
        .with_conn(move |c| {
            c.query_row(
                &format!("SELECT {JOB_COLS} FROM jobs WHERE id=?1"),
                params![id],
                job_from,
            )
            .optional()
        })
        .await
}

pub async fn upsert_job(store: &Store, job: JobRow) -> anyhow::Result<()> {
    let now = crate::paths::now_ms();
    store
        .with_conn(move |c| {
            c.execute(
                "INSERT INTO jobs(id,name,cron,mode,natural_text,project_id,branch,agent,gateway,enabled,prompt,notify_success,notify_fail,pre_check,created_at) \
                 VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15) \
                 ON CONFLICT(id) DO UPDATE SET \
                   name=excluded.name, cron=excluded.cron, mode=excluded.mode, \
                   natural_text=excluded.natural_text, project_id=excluded.project_id, \
                   branch=excluded.branch, agent=excluded.agent, gateway=excluded.gateway, \
                   enabled=excluded.enabled, prompt=excluded.prompt, \
                   notify_success=excluded.notify_success, notify_fail=excluded.notify_fail, \
                   pre_check=excluded.pre_check",
                params![
                    job.id, job.name, job.cron, job.mode, job.natural_text, job.project_id,
                    job.branch, job.agent, job.gateway, job.enabled as i64, job.prompt,
                    job.notify_success as i64, job.notify_fail as i64, job.pre_check, now
                ],
            )
            .map(|_| ())
        })
        .await
}

pub async fn delete_job(store: &Store, id: &str) -> anyhow::Result<()> {
    let id = id.to_string();
    store
        .with_conn(move |c| {
            c.execute("DELETE FROM job_runs WHERE job_id=?1", params![id])?;
            c.execute("DELETE FROM jobs WHERE id=?1", params![id])
                .map(|_| ())
        })
        .await
}

const RUN_COLS: &str =
    "id,job_id,status,started_at,finished_at,session_pk,error,add_lines,del_lines,note,log";

fn run_from(r: &rusqlite::Row) -> rusqlite::Result<RunRow> {
    Ok(RunRow {
        id: r.get(0)?,
        job_id: r.get(1)?,
        status: r.get(2)?,
        started_at: r.get(3)?,
        finished_at: r.get(4)?,
        session_pk: r.get(5)?,
        error: r.get(6)?,
        add_lines: r.get(7)?,
        del_lines: r.get(8)?,
        note: r.get(9)?,
        log: r.get(10)?,
    })
}

pub async fn list_runs(store: &Store, job_id: &str, limit: u32) -> anyhow::Result<Vec<RunRow>> {
    let job_id = job_id.to_string();
    store
        .with_conn(move |c| {
            let mut stmt = c.prepare(&format!(
                "SELECT {RUN_COLS} FROM job_runs WHERE job_id=?1 ORDER BY started_at DESC LIMIT ?2"
            ))?;
            let rows = stmt
                .query_map(params![job_id, limit], run_from)?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            Ok(rows)
        })
        .await
}

pub async fn insert_run(store: &Store, run: RunRow) -> anyhow::Result<()> {
    store
        .with_conn(move |c| {
            c.execute(
                &format!(
                    "INSERT INTO job_runs({RUN_COLS}) VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11)"
                ),
                params![
                    run.id,
                    run.job_id,
                    run.status,
                    run.started_at,
                    run.finished_at,
                    run.session_pk,
                    run.error,
                    run.add_lines,
                    run.del_lines,
                    run.note,
                    run.log
                ],
            )
            .map(|_| ())
        })
        .await
}

#[allow(clippy::too_many_arguments)]
pub async fn finalize_run(
    store: &Store,
    run_id: &str,
    status: &str,
    finished_at: i64,
    session_pk: Option<String>,
    error: Option<String>,
    add_lines: Option<i64>,
    del_lines: Option<i64>,
    note: Option<String>,
) -> anyhow::Result<()> {
    let run_id = run_id.to_string();
    let status = status.to_string();
    store
        .with_conn(move |c| {
            c.execute(
                "UPDATE job_runs SET status=?2, finished_at=?3, session_pk=COALESCE(?4, session_pk), \
                 error=?5, add_lines=?6, del_lines=?7, note=?8 WHERE id=?1",
                params![run_id, status, finished_at, session_pk, error, add_lines, del_lines, note],
            )
            .map(|_| ())
        })
        .await
}

/// Whether the job has a run still marked running (guards double-fires).
pub async fn has_running_run(store: &Store, job_id: &str) -> anyhow::Result<bool> {
    let job_id = job_id.to_string();
    store
        .with_conn(move |c| {
            c.query_row(
                "SELECT COUNT(*) FROM job_runs WHERE job_id=?1 AND status='running'",
                params![job_id],
                |r| r.get::<_, i64>(0),
            )
            .map(|n| n > 0)
        })
        .await
}

// ---------------------------------------------------------------------------
// Silence + wake gates (hermes-agent cron conventions)
// ---------------------------------------------------------------------------

/// Prompt header teaching scheduled sessions the silence convention.
pub const SCHED_HEADER: &str = "[Scheduled run] If, after checking, there is nothing worth \
reporting or doing, reply with a single line starting with [SILENT] - the run is still \
recorded but no notification is delivered.";

/// Whether a scheduled run's final reply opts out of delivery.
pub(crate) fn is_silent(text: &str) -> bool {
    text.trim_start().starts_with("[SILENT]")
}

/// The (notify, note) decision for a finished run's final assistant text.
pub(crate) fn run_note_for(final_text: Option<&str>) -> (bool, Option<String>) {
    match final_text {
        Some(t) if is_silent(t) => (false, Some("[SILENT] suppressed".to_string())),
        _ => (true, None),
    }
}

/// The final assistant message of a session: the trailing run of assistant
/// text rows (they are persisted delta-shaped), concatenated in order.
/// Shared with the orch dispatcher, which captures worker/judge reports.
pub(crate) async fn final_assistant_text(store: &Store, session_pk: &str) -> Option<String> {
    let msgs = store.list_messages(session_pk).await.ok()?;
    let mut parts: Vec<String> = Vec::new();
    for m in msgs.iter().rev() {
        if m.role == "assistant" && m.block_type == "text" {
            if let Some(t) = m.payload.get("text").and_then(|t| t.as_str()) {
                parts.push(t.to_string());
            }
        } else if m.role == "assistant" && m.block_type == "thought" {
            continue;
        } else {
            break;
        }
    }
    if parts.is_empty() {
        return None;
    }
    parts.reverse();
    Some(parts.concat())
}

/// Outcome of a job's wake-gate pre-check.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PreCheckOutcome {
    /// Nothing to do - skip this fire entirely (reason for the log).
    Skip(String),
    /// Wake the agent; stdout is appended to the job prompt.
    Wake(String),
}

/// Run a job's `pre_check` command (60s cap; `cmd /C` on Windows, `sh -c`
/// elsewhere) in `workdir` (the job's project checkout) so repo-relative
/// checks evaluate against the right tree. Empty stdout, non-zero exit,
/// spawn failure, or timeout skips the fire.
pub async fn run_pre_check(cmd: &str, workdir: Option<&str>) -> PreCheckOutcome {
    let mut c = if cfg!(windows) {
        let mut c = tokio::process::Command::new("cmd");
        c.args(["/C", cmd]);
        c
    } else {
        let mut c = tokio::process::Command::new("sh");
        c.args(["-c", cmd]);
        c
    };
    if let Some(dir) = workdir {
        c.current_dir(dir);
    }
    // A timed-out future is dropped: without kill_on_drop the child would
    // keep running detached (the spawn convention everywhere else in core).
    c.kill_on_drop(true);
    match tokio::time::timeout(Duration::from_secs(60), c.output()).await {
        Err(_) => PreCheckOutcome::Skip("pre-check timed out after 60s".into()),
        Ok(Err(e)) => PreCheckOutcome::Skip(format!("pre-check failed to spawn: {e}")),
        Ok(Ok(o)) if !o.status.success() => {
            PreCheckOutcome::Skip(format!("pre-check exited with {}", o.status))
        }
        Ok(Ok(o)) => {
            let stdout = String::from_utf8_lossy(&o.stdout).trim().to_string();
            if stdout.is_empty() {
                PreCheckOutcome::Skip("pre-check produced no output".into())
            } else {
                PreCheckOutcome::Wake(stdout)
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Runner
// ---------------------------------------------------------------------------

/// Sum of `git diff --numstat HEAD` in `workdir` → (added, deleted).
pub async fn diff_totals(workdir: &str) -> Option<(i64, i64)> {
    let out = tokio::process::Command::new("git")
        .args(["-C", workdir, "diff", "--numstat", "HEAD"])
        .output()
        .await
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let text = String::from_utf8_lossy(&out.stdout);
    let mut add = 0i64;
    let mut del = 0i64;
    for line in text.lines() {
        let mut cols = line.split_whitespace();
        add += cols.next().and_then(|v| v.parse::<i64>().ok()).unwrap_or(0);
        del += cols.next().and_then(|v| v.parse::<i64>().ok()).unwrap_or(0);
    }
    Some((add, del))
}

/// Execute `job` now (a MANUAL run: no scheduled-run header, so the agent is
/// not taught the [SILENT] convention and a user-triggered run always
/// notifies): create the run row, start the agent session, and close the run
/// when the session's first turn completes. Returns the run id.
pub async fn execute_job(cp: &Arc<ControlPlane>, job: &JobRow) -> anyhow::Result<String> {
    run_job(cp, job, job.prompt.clone()).await
}

/// Execute a SCHEDULED fire: the prompt gains the [`SCHED_HEADER`] silence
/// convention plus any wake-gate pre-check output.
pub async fn execute_job_scheduled(
    cp: &Arc<ControlPlane>,
    job: &JobRow,
    pre_check_output: Option<String>,
) -> anyhow::Result<String> {
    let mut prompt = format!("{SCHED_HEADER}\n\n{}", job.prompt);
    if let Some(out) = &pre_check_output {
        prompt.push_str(&format!("\n\nPre-check output:\n{out}"));
    }
    run_job(cp, job, prompt).await
}

async fn run_job(cp: &Arc<ControlPlane>, job: &JobRow, prompt: String) -> anyhow::Result<String> {
    let store = cp.store().clone();
    let run_id = format!("r-{}", &crate::paths::new_id()[..8]);
    let started = crate::paths::now_ms();
    insert_run(
        &store,
        RunRow {
            id: run_id.clone(),
            job_id: job.id.clone(),
            status: "running".into(),
            started_at: started,
            finished_at: None,
            session_pk: None,
            error: None,
            add_lines: None,
            del_lines: None,
            note: None,
            log: None,
        },
    )
    .await?;
    let _ = crate::gateways::add_event(
        &store,
        &job.gateway,
        "info",
        &format!("job {} run {run_id} started", job.name),
    )
    .await;

    // Subscribe BEFORE starting so a fast turn can't slip past the listener.
    let mut rx = cp.subscribe();
    let session = match cp
        .start_session(&job.project_id, &prompt, "scheduler", &[])
        .await
    {
        Ok(s) => s,
        Err(e) => {
            let now = crate::paths::now_ms();
            finalize_run(
                &store,
                &run_id,
                "failed",
                now,
                None,
                Some(e.to_string()),
                None,
                None,
                None,
            )
            .await?;
            let _ = crate::gateways::add_event(
                &store,
                &job.gateway,
                "error",
                &format!("job {} run {run_id} failed to start: {e}", job.name),
            )
            .await;
            let _ = cp.send_event(CoreEvent::JobRunChanged {
                job_id: job.id.clone(),
                run_id: run_id.clone(),
                status: "failed".into(),
            });
            return Ok(run_id);
        }
    };

    let session_pk = session.session_pk.clone();
    let worktree = session.worktree_path.clone();
    let job_id = job.id.clone();
    let job_name = job.name.clone();
    let gateway = job.gateway.clone();
    let cp2 = Arc::clone(cp);
    let run_id2 = run_id.clone();
    tokio::spawn(async move {
        let deadline = tokio::time::Instant::now() + Duration::from_secs(2 * 60 * 60);
        let mut outcome: (&str, Option<String>) = ("failed", Some("run watcher stopped".into()));
        loop {
            let ev = tokio::time::timeout_at(deadline, rx.recv()).await;
            match ev {
                Ok(Ok(CoreEvent::Result { session_pk: pk })) if pk == session_pk => {
                    outcome = ("success", None);
                    break;
                }
                Ok(Ok(CoreEvent::Error {
                    session_pk: pk,
                    message,
                })) if pk == session_pk => {
                    outcome = ("failed", Some(message));
                    break;
                }
                Ok(Ok(_)) => continue,
                Ok(Err(tokio::sync::broadcast::error::RecvError::Lagged(_))) => continue,
                Ok(Err(_)) => break,
                Err(_) => {
                    outcome = ("failed", Some("timed out after 2h".into()));
                    break;
                }
            }
        }
        let (status, error) = outcome;
        let (add, del) = match &worktree {
            Some(wt) if status == "success" => diff_totals(wt).await.unwrap_or((0, 0)),
            _ => (0, 0),
        };
        let now = crate::paths::now_ms();
        let final_text = if status == "success" {
            final_assistant_text(cp2.store(), &session_pk).await
        } else {
            None
        };
        let (notify, silent_note) = run_note_for(final_text.as_deref());
        let note = silent_note.or_else(|| {
            if status == "success" && add == 0 && del == 0 {
                Some("No changes produced".to_string())
            } else {
                None
            }
        });
        let _ = finalize_run(
            cp2.store(),
            &run_id2,
            status,
            now,
            Some(session_pk.clone()),
            error.clone(),
            Some(add),
            Some(del),
            note,
        )
        .await;
        if status != "success" || notify {
            let level = if status == "success" {
                "success"
            } else {
                "error"
            };
            let text = match &error {
                Some(e) => format!("job {job_name} run {run_id2} failed — {e}"),
                None => format!("job {job_name} run {run_id2} finished — +{add} −{del}"),
            };
            let _ = crate::gateways::add_event(cp2.store(), &gateway, level, &text).await;
        }
        let _ = cp2.send_event(CoreEvent::JobRunChanged {
            job_id,
            run_id: run_id2,
            status: status.into(),
        });
    });

    // Record the session on the run row right away so the UI can link to it.
    finalize_partial_session(&store, &run_id, &session.session_pk).await?;
    Ok(run_id)
}

async fn finalize_partial_session(
    store: &Store,
    run_id: &str,
    session_pk: &str,
) -> anyhow::Result<()> {
    let run_id = run_id.to_string();
    let session_pk = session_pk.to_string();
    store
        .with_conn(move |c| {
            c.execute(
                "UPDATE job_runs SET session_pk=?2 WHERE id=?1",
                params![run_id, session_pk],
            )
            .map(|_| ())
        })
        .await
}

/// Background loop: every 30s, fire enabled jobs whose next occurrence (after
/// the last fire) has passed. `last fired` persists in settings KV so app
/// restarts don't re-fire missed-by-restart schedules more than once.
///
/// Returned as a future (not self-spawned) so hosts can run it on their own
/// runtime — Tauri's setup hook has no ambient tokio context.
pub fn spawn_runner(cp: Arc<ControlPlane>) -> tokio::task::JoinHandle<()> {
    tokio::spawn(run_loop(cp))
}

pub async fn run_loop(cp: Arc<ControlPlane>) {
    loop {
        tokio::time::sleep(Duration::from_secs(30)).await;
        tick(&cp).await;
    }
}

/// One scheduler pass: record liveness, then fire any due jobs (through their
/// wake-gate pre-checks). Factored out of [`run_loop`] so tests can drive it
/// without sleeping.
pub async fn tick(cp: &Arc<ControlPlane>) {
    let store = cp.store().clone();
    let now = crate::paths::now_ms();
    // Cheap staleness probe for health surfaces.
    let _ = store
        .set_setting("scheduler_last_tick", &now.to_string())
        .await;
    let jobs = match list_jobs(&store).await {
        Ok(j) => j,
        Err(_) => return,
    };
    for job in jobs.into_iter().filter(|j| j.enabled) {
        let key = format!("job_last_fired.{}", job.id);
        let last_fired: i64 = store
            .get_setting(&key)
            .await
            .ok()
            .flatten()
            .and_then(|v| v.parse().ok())
            // First sighting: anchor at now so we fire on the NEXT occurrence.
            .unwrap_or(now);
        if last_fired == now {
            let _ = store.set_setting(&key, &now.to_string()).await;
            continue;
        }
        let Some(next) = next_run_after(&job.cron, last_fired) else {
            continue;
        };
        if next > now {
            continue;
        }
        if has_running_run(&store, &job.id).await.unwrap_or(true) {
            continue;
        }
        let _ = store.set_setting(&key, &now.to_string()).await;
        // Fire on a detached task: a slow/hung pre-check (up to 60s) must not
        // stall the other due jobs or the next liveness stamp. The anchor is
        // already advanced, so this fire cannot double-run.
        let cp2 = cp.clone();
        tokio::spawn(async move {
            // Wake gate: a configured pre-check must produce output, or the
            // fire is skipped entirely (no session, no run row).
            let pre = if job.pre_check.trim().is_empty() {
                None
            } else {
                let workdir = cp2
                    .store()
                    .get_project(&job.project_id)
                    .await
                    .ok()
                    .flatten()
                    .map(|p| p.workdir);
                match run_pre_check(&job.pre_check, workdir.as_deref()).await {
                    PreCheckOutcome::Skip(reason) => {
                        tracing::debug!("scheduler: job {} skipped ({reason})", job.id);
                        return;
                    }
                    PreCheckOutcome::Wake(out) => Some(out),
                }
            };
            let _ = execute_job_scheduled(&cp2, &job, pre).await;
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn natural_phrases_map_to_cron() {
        assert_eq!(
            natural_to_cron("every day at 2am").as_deref(),
            Some("0 2 * * *")
        );
        assert_eq!(
            natural_to_cron("every day at 14:30").as_deref(),
            Some("30 14 * * *")
        );
        assert_eq!(
            natural_to_cron("every monday at 9am").as_deref(),
            Some("0 9 * * 1")
        );
        assert_eq!(
            natural_to_cron("weekdays at 9:15am").as_deref(),
            Some("15 9 * * 1-5")
        );
        assert_eq!(
            natural_to_cron("every 6 hours").as_deref(),
            Some("0 */6 * * *")
        );
        assert_eq!(
            natural_to_cron("every 15 minutes").as_deref(),
            Some("*/15 * * * *")
        );
        assert_eq!(
            natural_to_cron("every day at 12am").as_deref(),
            Some("0 0 * * *")
        );
        assert_eq!(
            natural_to_cron("every day at 12pm").as_deref(),
            Some("0 12 * * *")
        );
        assert_eq!(natural_to_cron("whenever I feel like it"), None);
        assert_eq!(natural_to_cron("every day at 25:00"), None);
    }

    #[test]
    fn next_run_is_strictly_after_anchor() {
        // Daily at 02:00 — anchor at some fixed time; next must be within 24h and after.
        let now = crate::paths::now_ms();
        let next = next_run_after("0 2 * * *", now).expect("valid cron");
        assert!(next > now);
        assert!(next - now <= 24 * 60 * 60 * 1000 + 60_000);
        assert!(next_run_after("not a cron", now).is_none());
    }

    #[test]
    fn silent_prefix_detection_and_note() {
        assert!(is_silent("[SILENT] nothing to do"));
        assert!(is_silent("  [SILENT]"));
        assert!(!is_silent("done: [SILENT] not a prefix"));
        assert!(!is_silent("all good"));
        assert_eq!(
            run_note_for(Some("[SILENT] ok")),
            (false, Some("[SILENT] suppressed".to_string()))
        );
        assert_eq!(run_note_for(Some("did things")), (true, None));
        assert_eq!(run_note_for(None), (true, None));
    }

    #[tokio::test]
    async fn pre_check_gates_on_output_and_exit() {
        assert_eq!(
            run_pre_check("echo hi", None).await,
            PreCheckOutcome::Wake("hi".into())
        );
        assert!(matches!(
            run_pre_check("exit 1", None).await,
            PreCheckOutcome::Skip(_)
        ));
        // Succeeds but prints nothing: still a skip.
        let quiet = if cfg!(windows) { "rem quiet" } else { "true" };
        assert!(matches!(
            run_pre_check(quiet, None).await,
            PreCheckOutcome::Skip(_)
        ));
        // The command runs in the given workdir.
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("flag.txt"), "x").unwrap();
        let list = if cfg!(windows) {
            "dir /b flag.txt"
        } else {
            "ls flag.txt"
        };
        assert_eq!(
            run_pre_check(list, Some(&dir.path().to_string_lossy())).await,
            PreCheckOutcome::Wake("flag.txt".into())
        );
    }

    #[tokio::test]
    async fn tick_records_scheduler_liveness() {
        let tmp = tempfile::NamedTempFile::new().unwrap();
        let store = Store::open(tmp.path()).await.unwrap();
        let cp = crate::control::ControlPlane::new(store, crate::plugins::Registries::new()).await;
        tick(&cp).await;
        let val = cp
            .store()
            .get_setting("scheduler_last_tick")
            .await
            .unwrap()
            .expect("liveness recorded");
        assert!(val.parse::<i64>().unwrap() > 0);
    }

    #[tokio::test]
    async fn job_and_run_crud_roundtrip() {
        let tmp = tempfile::NamedTempFile::new().unwrap();
        let store = Store::open(tmp.path()).await.unwrap();

        let job = JobRow {
            id: "j1".into(),
            name: "Nightly audit".into(),
            cron: "0 2 * * *".into(),
            mode: "natural".into(),
            natural_text: "every day at 2am".into(),
            project_id: "p1".into(),
            branch: "main".into(),
            agent: "claude".into(),
            gateway: "local".into(),
            enabled: true,
            prompt: "Run npm audit".into(),
            notify_success: false,
            notify_fail: true,
            pre_check: "git status --short".into(),
        };
        upsert_job(&store, job.clone()).await.unwrap();
        assert_eq!(get_job(&store, "j1").await.unwrap().unwrap(), job);

        insert_run(
            &store,
            RunRow {
                id: "r1".into(),
                job_id: "j1".into(),
                status: "running".into(),
                started_at: 1000,
                finished_at: None,
                session_pk: None,
                error: None,
                add_lines: None,
                del_lines: None,
                note: None,
                log: None,
            },
        )
        .await
        .unwrap();
        assert!(has_running_run(&store, "j1").await.unwrap());

        finalize_run(
            &store,
            "r1",
            "success",
            2000,
            Some("s-1".into()),
            None,
            Some(12),
            Some(4),
            None,
        )
        .await
        .unwrap();
        let runs = list_runs(&store, "j1", 10).await.unwrap();
        assert_eq!(runs.len(), 1);
        assert_eq!(runs[0].status, "success");
        assert_eq!(runs[0].add_lines, Some(12));
        assert_eq!(runs[0].session_pk.as_deref(), Some("s-1"));
        assert!(!has_running_run(&store, "j1").await.unwrap());

        delete_job(&store, "j1").await.unwrap();
        assert!(get_job(&store, "j1").await.unwrap().is_none());
        assert!(list_runs(&store, "j1", 10).await.unwrap().is_empty());
    }
}
