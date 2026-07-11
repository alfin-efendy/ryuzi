//! Scheduler screen commands. Jobs persist in SQLite; the core runner loop
//! fires them for real (starting agent sessions); run history closes off the
//! session's Result/Error events. Moved verbatim (per the Move Recipe) from
//! `apps/cockpit/src-tauri/src/scheduler_cmd.rs`; that file keeps its own
//! copy until the proxy rewrite in Tasks 15-16. `parse_natural_schedule` does
//! NOT move — it's a pure wrapper around `scheduler::natural_to_cron` that
//! Cockpit still calls directly against ryuzi-core as a library.

use super::{ok, params, ApiError};
use crate::api::types::*;
use crate::control::ControlPlane;
use crate::scheduler::{self, JobRow};
use crate::serve::ApiState;
use serde::Deserialize;
use serde_json::Value;

pub(crate) const HANDLES: &[&str] = &[
    "list_jobs",
    "create_job",
    "update_job",
    "toggle_job",
    "delete_job",
    "run_job_now",
];

#[derive(Deserialize)]
struct InputP {
    input: JobInput,
}
#[derive(Deserialize)]
struct IdInputP {
    id: String,
    input: JobInput,
}
#[derive(Deserialize)]
struct IdEnabledP {
    id: String,
    enabled: bool,
}
#[derive(Deserialize)]
struct IdP {
    id: String,
}

pub(crate) async fn dispatch(state: &ApiState, method: &str, p: Value) -> Result<Value, ApiError> {
    let cp = &state.cp;
    match method {
        "list_jobs" => ok(assemble(cp).await?),
        "create_job" => {
            let a: InputP = params(p)?;
            ok(create_job(state, a.input).await?)
        }
        "update_job" => {
            let a: IdInputP = params(p)?;
            ok(update_job(state, a.id, a.input).await?)
        }
        "toggle_job" => {
            let a: IdEnabledP = params(p)?;
            ok(toggle_job(state, a.id, a.enabled).await?)
        }
        "delete_job" => {
            let a: IdP = params(p)?;
            scheduler::delete_job(cp.store(), &a.id).await?;
            ok(assemble(cp).await?)
        }
        "run_job_now" => {
            let a: IdP = params(p)?;
            ok(run_job_now(state, &a.id).await?)
        }
        _ => Err(ApiError::not_found(format!("unknown method: {method}"))),
    }
}

fn resolve_cron(input: &JobInput) -> Result<String, ApiError> {
    let cron = if input.mode == "natural" {
        scheduler::natural_to_cron(&input.natural).ok_or_else(|| {
            ApiError::bad_request(format!(
                "couldn't parse \"{}\" — try e.g. \"every day at 2am\", \"every monday at 9am\", \"every 6 hours\", or switch to cron mode",
                input.natural
            ))
        })?
    } else {
        input.cron.trim().to_string()
    };
    // Validate by computing the next occurrence.
    scheduler::next_run_after(&cron, crate::paths::now_ms())
        .ok_or_else(|| ApiError::bad_request(format!("invalid cron expression: {cron}")))?;
    Ok(cron)
}

/// Wall-clock duration of a run; a run that hasn't finished has none yet.
fn run_duration_ms(started_at: i64, finished_at: Option<i64>) -> Option<i64> {
    finished_at.map(|f| f - started_at)
}

async fn assemble(cp: &ControlPlane) -> anyhow::Result<Vec<JobInfo>> {
    let projects = cp.list_projects().await?;
    let now = crate::paths::now_ms();
    let mut out = Vec::new();
    for job in scheduler::list_jobs(cp.store()).await? {
        let runs = scheduler::list_runs(cp.store(), &job.id, 20).await?;
        let project_name = projects
            .iter()
            .find(|p| p.project_id == job.project_id)
            .map(|p| p.name.clone())
            .unwrap_or_else(|| job.project_id.clone());
        out.push(JobInfo {
            next_run_ms: if job.enabled {
                scheduler::next_run_after(&job.cron, now)
            } else {
                None
            },
            id: job.id,
            name: job.name,
            cron: job.cron,
            mode: job.mode,
            natural: job.natural_text,
            project_id: job.project_id,
            project_name,
            branch: job.branch,
            gateway: job.gateway,
            enabled: job.enabled,
            prompt: job.prompt,
            notify_success: job.notify_success,
            notify_fail: job.notify_fail,
            history: runs
                .into_iter()
                .map(|r| RunInfo {
                    id: r.id,
                    status: r.status,
                    started_at_ms: r.started_at,
                    duration_ms: run_duration_ms(r.started_at, r.finished_at),
                    add_lines: r.add_lines,
                    del_lines: r.del_lines,
                    note: r.note,
                    error: r.error,
                    session_pk: r.session_pk,
                })
                .collect(),
        });
    }
    Ok(out)
}

async fn create_job(state: &ApiState, input: JobInput) -> Result<Vec<JobInfo>, ApiError> {
    let cp = &state.cp;
    let cron = resolve_cron(&input)?;
    let id = format!("j-{}", &crate::paths::new_id()[..8]);
    scheduler::upsert_job(
        cp.store(),
        JobRow {
            id,
            name: input.name,
            cron,
            mode: input.mode,
            natural_text: input.natural,
            project_id: input.project_id,
            branch: input.branch,
            gateway: input.gateway,
            enabled: true,
            prompt: input.prompt,
            notify_success: input.notify_success,
            notify_fail: input.notify_fail,
            // Wake-gate editing lands with the scheduler panel rework.
            pre_check: String::new(),
        },
    )
    .await?;
    Ok(assemble(cp).await?)
}

async fn update_job(
    state: &ApiState,
    id: String,
    input: JobInput,
) -> Result<Vec<JobInfo>, ApiError> {
    let cp = &state.cp;
    let existing = scheduler::get_job(cp.store(), &id)
        .await?
        .ok_or_else(|| ApiError::not_found(format!("unknown job: {id}")))?;
    let cron = resolve_cron(&input)?;
    scheduler::upsert_job(
        cp.store(),
        JobRow {
            id,
            name: input.name,
            cron,
            mode: input.mode,
            natural_text: input.natural,
            project_id: input.project_id,
            branch: input.branch,
            gateway: input.gateway,
            enabled: existing.enabled,
            prompt: input.prompt,
            notify_success: input.notify_success,
            notify_fail: input.notify_fail,
            pre_check: existing.pre_check.clone(),
        },
    )
    .await?;
    Ok(assemble(cp).await?)
}

async fn toggle_job(state: &ApiState, id: String, enabled: bool) -> Result<Vec<JobInfo>, ApiError> {
    let cp = &state.cp;
    let mut job = scheduler::get_job(cp.store(), &id)
        .await?
        .ok_or_else(|| ApiError::not_found(format!("unknown job: {id}")))?;
    job.enabled = enabled;
    scheduler::upsert_job(cp.store(), job).await?;
    // Re-anchor so enabling doesn't immediately fire a long-past occurrence.
    cp.store()
        .set_setting(
            &format!("job_last_fired.{id}"),
            &crate::paths::now_ms().to_string(),
        )
        .await?;
    Ok(assemble(cp).await?)
}

async fn run_job_now(state: &ApiState, id: &str) -> Result<Vec<JobInfo>, ApiError> {
    let cp = &state.cp;
    let job = scheduler::get_job(cp.store(), id)
        .await?
        .ok_or_else(|| ApiError::not_found(format!("unknown job: {id}")))?;
    if scheduler::has_running_run(cp.store(), id).await? {
        return Err(ApiError::bad_request(
            "a run is already in progress for this job",
        ));
    }
    scheduler::execute_job(cp, &job).await?;
    Ok(assemble(cp).await?)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::{dispatch, tests_support::state};
    use serde_json::json;

    fn input(mode: &str, natural: &str, cron: &str) -> JobInput {
        JobInput {
            name: "job".into(),
            mode: mode.into(),
            natural: natural.into(),
            cron: cron.into(),
            project_id: "p1".into(),
            branch: "main".into(),
            gateway: "local".into(),
            prompt: "do it".into(),
            notify_success: false,
            notify_fail: false,
        }
    }

    #[test]
    fn natural_mode_translates_english_and_ignores_the_cron_field() {
        let cron = resolve_cron(&input("natural", "every day at 2am", "9 9 9 9 9")).unwrap();
        assert_eq!(cron, "0 2 * * *");
    }

    #[test]
    fn unparseable_natural_text_suggests_alternatives() {
        let err = resolve_cron(&input("natural", "whenever it rains", "")).unwrap_err();
        assert_eq!(
            err.message,
            "couldn't parse \"whenever it rains\" — try e.g. \"every day at 2am\", \"every monday at 9am\", \"every 6 hours\", or switch to cron mode"
        );
    }

    #[test]
    fn cron_mode_trims_and_keeps_the_expression() {
        let cron = resolve_cron(&input("cron", "ignored", "  0 2 * * *  ")).unwrap();
        assert_eq!(cron, "0 2 * * *");
    }

    #[test]
    fn cron_mode_rejects_invalid_expressions() {
        let err = resolve_cron(&input("cron", "", "not a cron")).unwrap_err();
        assert_eq!(err.message, "invalid cron expression: not a cron");
    }

    #[test]
    fn duration_is_finish_minus_start() {
        assert_eq!(run_duration_ms(1_000, Some(4_500)), Some(3_500));
    }

    #[test]
    fn unfinished_run_has_no_duration() {
        assert_eq!(run_duration_ms(1_000, None), None);
    }

    #[tokio::test]
    async fn job_crud_round_trip_via_rpc() {
        let s = state().await;
        let jobs = dispatch(
            &s,
            "create_job",
            json!({ "input": {
                "name": "nightly", "mode": "cron", "natural": "", "cron": "0 3 * * *",
                "projectId": "p1", "branch": "", "gateway": "",
                "prompt": "check things", "notifySuccess": false, "notifyFail": true
            }}),
        )
        .await
        .unwrap();
        let id = jobs[0]["id"].as_str().unwrap().to_string();
        let toggled = dispatch(&s, "toggle_job", json!({"id": id, "enabled": false}))
            .await
            .unwrap();
        assert_eq!(toggled[0]["enabled"], false);
        let after = dispatch(&s, "delete_job", json!({"id": id})).await.unwrap();
        assert_eq!(after, json!([]));
    }
}
