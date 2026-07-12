//! Daemon-hosted curator loop (spec §7.5): weekly deterministic skill
//! lifecycle transitions over `skill_usage` (Task 5's counters/timestamps) —
//! `active` → `stale` after ~30 days unused, `stale` → `archived` after ~90
//! days unused, and reactivate-on-use (`stale`/`archived` → `active` the
//! instant a skill is used again). Pinned skills (`skill_usage.pinned`) and
//! skills referenced by any scheduler job's prompt are exempt from aging —
//! a cron job can invoke a skill by name on its own cadence regardless of
//! how `skill_usage` looks. Newly-tracked skills get a grace floor
//! (`GRACE_DAYS`) before they're eligible to stale or archive at all.
//!
//! This is a DEDICATED daemon loop, not a `jobs` row: every scheduler job
//! requires a `project_id` and surfaces in the user-facing Scheduler UI,
//! neither of which fits a headless housekeeping sweep with no project and
//! nothing for a human to review or approve.
//!
//! The decision table is a pure function, [`plan_transitions`], exhaustively
//! unit-tested against a fixed `now` (never a wall-clock call) — kept
//! separate from the loop (`tick`/`run_loop`/`spawn_runner`, mirroring
//! `learning.rs`) that supplies the real store/scheduler I/O.

use crate::domain::SkillUsage;
use crate::store::Store;
use std::collections::{BTreeMap, HashSet};
use std::sync::Arc;
use std::time::Duration;

/// How often the loop itself wakes to check whether a sweep is due. This is
/// NOT the sweep cadence — see `INTERVAL_DAYS`/`curator_last_run` for that.
const POLL_INTERVAL: Duration = Duration::from_secs(3_600);

/// Curator sweep cadence: once a week.
const INTERVAL_DAYS: i64 = 7;

/// A skill unused this long transitions `active` → `stale`.
const STALE_AFTER_DAYS: i64 = 30;
/// A skill unused this long transitions (`active` or `stale`) → `archived`.
const ARCHIVE_AFTER_DAYS: i64 = 90;
/// A skill younger than this (by `skill_usage.created_at`) is never staled
/// or archived, no matter how old its usage anchor looks.
const GRACE_DAYS: i64 = 14;

const DAY_MS: i64 = 86_400_000;

/// Deterministic lifecycle transitions (spec §7.5). Returns `name` → new
/// `state` for every skill that should change; a skill absent from the map
/// is left exactly as it is. Pure so the decision table is unit-testable
/// without a database or a clock.
///
/// - `pinned` or `cron_referenced` skills are never touched.
/// - The "usage anchor" is `last_used_at`, falling back to `created_at` for
///   a skill that has never been used.
/// - `active` ages to `archived` at `ARCHIVE_AFTER_DAYS`, to `stale` at
///   `STALE_AFTER_DAYS` — but only once the skill has existed for at least
///   `GRACE_DAYS` (the young-skill floor).
/// - `stale` ages to `archived` at `ARCHIVE_AFTER_DAYS` (no grace check —
///   grace only protects a skill from going stale/archived in the first
///   place, not from continuing to age once it already has).
/// - `stale`/`archived` reactivate to `active` the moment `last_used_at` is
///   within `STALE_AFTER_DAYS` of `now` — a use is always enough to bring a
///   skill back, regardless of grace.
pub fn plan_transitions(
    rows: &[SkillUsage],
    cron_referenced: &HashSet<String>,
    now: i64,
) -> BTreeMap<String, String> {
    let mut plan = BTreeMap::new();
    for s in rows {
        if s.pinned || cron_referenced.contains(&s.name) {
            continue;
        }
        let anchor = s.last_used_at.or(s.created_at).unwrap_or(now);
        let age_days = (now - anchor) / DAY_MS;
        let created_age = (now - s.created_at.unwrap_or(now)) / DAY_MS;
        let next = match s.state.as_str() {
            "active" if age_days >= ARCHIVE_AFTER_DAYS && created_age >= GRACE_DAYS => {
                Some("archived")
            }
            "active" if age_days >= STALE_AFTER_DAYS && created_age >= GRACE_DAYS => Some("stale"),
            "stale" if age_days >= ARCHIVE_AFTER_DAYS => Some("archived"),
            "stale" | "archived"
                if s.last_used_at
                    .map(|u| (now - u) / DAY_MS < STALE_AFTER_DAYS)
                    .unwrap_or(false) =>
            {
                Some("active")
            }
            _ => None,
        };
        if let Some(n) = next {
            plan.insert(s.name.clone(), n.to_string());
        }
    }
    plan
}

/// Skill names referenced by ANY scheduler job's prompt (enabled or not — a
/// disabled job can be re-enabled at any time, so its skill reference stays
/// protected in the meantime), exempt from curator aging the same as a pin.
/// A simple substring containment check: good enough to protect a skill a
/// job's prompt names, with no false-negative risk from stemming/casing
/// mismatches costing a real skill its protection.
async fn cron_referenced_skills(store: &Store, known: &[SkillUsage]) -> HashSet<String> {
    let jobs = crate::scheduler::list_jobs(store).await.unwrap_or_default();
    known
        .iter()
        .filter(|s| jobs.iter().any(|j| j.prompt.contains(&s.name)))
        .map(|s| s.name.clone())
        .collect()
}

/// One curator sweep: due-check, then (if due) plan + apply deterministic
/// transitions, recording a `curator_runs` row throughout. Factored out of
/// [`run_loop`] so tests can drive it without sleeping a week.
pub async fn tick(store: &Arc<Store>) {
    let now = crate::paths::now_ms();
    if let Ok(Some(last)) = store.curator_last_run().await {
        if now - last < INTERVAL_DAYS * DAY_MS {
            return; // not due yet
        }
    }

    let run_id = crate::paths::new_id();
    if store.insert_curator_run(&run_id, now).await.is_err() {
        // Couldn't even open the run row — nothing to record; try again
        // next tick rather than sweeping untracked.
        return;
    }

    let rows = match store.list_skill_usage().await {
        Ok(rows) => rows,
        Err(e) => {
            let _ = store
                .finish_curator_run(
                    &run_id,
                    crate::paths::now_ms(),
                    "error",
                    0,
                    false,
                    None,
                    Some(&e.to_string()),
                )
                .await;
            return;
        }
    };

    let cron_referenced = cron_referenced_skills(store, &rows).await;
    let plan = plan_transitions(&rows, &cron_referenced, now);

    let mut transitioned = 0i64;
    for (name, new_state) in &plan {
        let archived_at = (new_state == "archived").then_some(now);
        match store.set_skill_state(name, new_state, archived_at).await {
            Ok(()) => transitioned += 1,
            Err(e) => tracing::warn!("curator: failed to transition {name} to {new_state}: {e}"),
        }
    }

    let _ = store
        .finish_curator_run(
            &run_id,
            crate::paths::now_ms(),
            "ok",
            transitioned,
            false,
            None,
            None,
        )
        .await;
}

/// The curator's background loop: sleep, then check-and-maybe-sweep,
/// forever. Returned as a future (not self-spawned) so hosts can run it on
/// their own runtime, mirroring `learning::run_loop` / `background_rail::
/// run_loop` / `scheduler::run_loop` / `orch::run_loop`.
pub async fn run_loop(store: Arc<Store>) {
    loop {
        tokio::time::sleep(POLL_INTERVAL).await;
        tick(&store).await;
    }
}

/// Spawn the loop on the host's runtime — the daemon is the single
/// always-on engine host for it, same as every other background loop (see
/// `Daemon`'s `scheduler_handle` doc in `daemon.rs`).
pub fn spawn_runner(store: Arc<Store>) -> tokio::task::JoinHandle<()> {
    tokio::spawn(run_loop(store))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn usage(
        name: &str,
        state: &str,
        last_used_at: Option<i64>,
        pinned: bool,
        created_at: Option<i64>,
    ) -> SkillUsage {
        SkillUsage {
            name: name.to_string(),
            created_by: Some("agent".into()),
            use_count: 1,
            view_count: 0,
            patch_count: 0,
            last_used_at,
            last_viewed_at: None,
            last_patched_at: None,
            state: state.to_string(),
            pinned,
            archived_at: None,
            created_at,
        }
    }

    // ---------- plan_transitions: the brief's own load-bearing table ----------

    #[test]
    fn deterministic_transitions_age_and_protect() {
        let now = 1_000_000_000_000i64;
        let day = DAY_MS;
        let u = |name, state, last_used, pinned| {
            usage(name, state, last_used, pinned, Some(now - 100 * day))
        };
        let rows = vec![
            u("fresh", "active", Some(now - 5 * day), false),
            u("stale", "active", Some(now - 40 * day), false),
            u("old", "active", Some(now - 100 * day), false),
            u("pinned", "active", Some(now - 100 * day), true),
        ];
        let cron_refs: HashSet<String> = Default::default();
        let plan = plan_transitions(&rows, &cron_refs, now);
        assert_eq!(plan.get("fresh"), None);
        assert_eq!(plan.get("stale").map(String::as_str), Some("stale"));
        assert_eq!(plan.get("old").map(String::as_str), Some("archived"));
        assert_eq!(plan.get("pinned"), None, "pinned skills are never aged");
    }

    // ---------- every remaining (state, age, pinned, used-since) cell ----------

    #[test]
    fn active_cron_referenced_is_never_aged_even_when_ancient() {
        let now = 1_000_000_000_000i64;
        let day = DAY_MS;
        let rows = vec![usage(
            "deploy",
            "active",
            Some(now - 200 * day),
            false,
            Some(now - 200 * day),
        )];
        let cron_refs: HashSet<String> = ["deploy".to_string()].into_iter().collect();
        let plan = plan_transitions(&rows, &cron_refs, now);
        assert_eq!(
            plan.get("deploy"),
            None,
            "cron-referenced skills are exempt from aging, same as pinned"
        );
    }

    #[test]
    fn stale_ages_to_archived_at_ninety_days() {
        let now = 1_000_000_000_000i64;
        let day = DAY_MS;
        let rows = vec![usage(
            "deploy",
            "stale",
            Some(now - 95 * day),
            false,
            Some(now - 200 * day),
        )];
        let plan = plan_transitions(&rows, &HashSet::new(), now);
        assert_eq!(plan.get("deploy").map(String::as_str), Some("archived"));
    }

    #[test]
    fn stale_under_ninety_days_and_not_recently_used_is_unchanged() {
        let now = 1_000_000_000_000i64;
        let day = DAY_MS;
        let rows = vec![usage(
            "deploy",
            "stale",
            Some(now - 45 * day),
            false,
            Some(now - 200 * day),
        )];
        let plan = plan_transitions(&rows, &HashSet::new(), now);
        assert_eq!(plan.get("deploy"), None);
    }

    #[test]
    fn stale_reactivates_to_active_on_recent_use() {
        let now = 1_000_000_000_000i64;
        let day = DAY_MS;
        let rows = vec![usage(
            "deploy",
            "stale",
            Some(now - 5 * day),
            false,
            Some(now - 200 * day),
        )];
        let plan = plan_transitions(&rows, &HashSet::new(), now);
        assert_eq!(plan.get("deploy").map(String::as_str), Some("active"));
    }

    #[test]
    fn archived_reactivates_to_active_on_recent_use() {
        let now = 1_000_000_000_000i64;
        let day = DAY_MS;
        let rows = vec![usage(
            "deploy",
            "archived",
            Some(now - 2 * day),
            false,
            Some(now - 200 * day),
        )];
        let plan = plan_transitions(&rows, &HashSet::new(), now);
        assert_eq!(plan.get("deploy").map(String::as_str), Some("active"));
    }

    #[test]
    fn archived_stays_archived_when_never_reactivated() {
        let now = 1_000_000_000_000i64;
        let day = DAY_MS;
        let never_used = usage("a", "archived", None, false, Some(now - 200 * day));
        let used_long_ago = usage(
            "b",
            "archived",
            Some(now - 60 * day),
            false,
            Some(now - 200 * day),
        );
        let plan = plan_transitions(&[never_used, used_long_ago], &HashSet::new(), now);
        assert_eq!(plan.get("a"), None);
        assert_eq!(plan.get("b"), None);
    }

    #[test]
    fn never_used_skill_ages_from_created_at() {
        let now = 1_000_000_000_000i64;
        let day = DAY_MS;
        let rows = vec![usage(
            "deploy",
            "active",
            None,
            false,
            Some(now - 100 * day),
        )];
        let plan = plan_transitions(&rows, &HashSet::new(), now);
        assert_eq!(
            plan.get("deploy").map(String::as_str),
            Some("archived"),
            "a never-used skill ages from created_at, same as a used one from last_used_at"
        );
    }

    #[test]
    fn young_skill_grace_floor_protects_against_staling_and_archiving() {
        let now = 1_000_000_000_000i64;
        let day = DAY_MS;
        // Anchor (last_used_at) looks ancient enough to archive, but the row
        // itself is only 5 days old (created_age < GRACE_DAYS) — the grace
        // floor must win.
        let rows = vec![usage(
            "deploy",
            "active",
            Some(now - 200 * day),
            false,
            Some(now - 5 * day),
        )];
        let plan = plan_transitions(&rows, &HashSet::new(), now);
        assert_eq!(
            plan.get("deploy"),
            None,
            "a skill younger than GRACE_DAYS must never stale or archive, \
             regardless of how old its usage anchor looks"
        );
    }

    #[test]
    fn pinned_stale_skill_is_never_touched_even_if_state_already_drifted() {
        let now = 1_000_000_000_000i64;
        let day = DAY_MS;
        let rows = vec![usage(
            "deploy",
            "stale",
            Some(now - 200 * day),
            true,
            Some(now - 200 * day),
        )];
        let plan = plan_transitions(&rows, &HashSet::new(), now);
        assert_eq!(
            plan.get("deploy"),
            None,
            "pinned skips before state is ever inspected"
        );
    }

    #[test]
    fn unknown_state_is_left_alone() {
        let now = 1_000_000_000_000i64;
        let day = DAY_MS;
        let rows = vec![usage(
            "deploy",
            "deprecated",
            Some(now - 200 * day),
            false,
            Some(now - 200 * day),
        )];
        let plan = plan_transitions(&rows, &HashSet::new(), now);
        assert_eq!(plan.get("deploy"), None);
    }

    // ---------- tick(): end-to-end against a real Store ----------

    #[tokio::test]
    async fn tick_is_a_noop_when_not_due() {
        let tmp = tempfile::NamedTempFile::new().unwrap();
        let store = Arc::new(Store::open(tmp.path()).await.unwrap());
        let now = crate::paths::now_ms();
        store.insert_curator_run("run-recent", now).await.unwrap();
        store
            .finish_curator_run("run-recent", now, "ok", 0, false, None, None)
            .await
            .unwrap();

        tick(&store).await;

        assert_eq!(
            store.list_curator_runs(10).await.unwrap().len(),
            1,
            "a run inside the weekly interval must not start a second sweep"
        );
    }

    #[tokio::test]
    async fn tick_runs_when_never_run_before_and_records_a_curator_run() {
        let tmp = tempfile::NamedTempFile::new().unwrap();
        let store = Arc::new(Store::open(tmp.path()).await.unwrap());
        let now = crate::paths::now_ms();
        let day = DAY_MS;
        // Seed one skill old enough to archive and one fresh enough to leave
        // alone, so this proves the whole claim -> plan -> apply -> finish
        // pipeline, not just the pure function.
        store.record_skill_use("ancient").await.unwrap();
        store
            .set_skill_state("ancient", "active", None)
            .await
            .unwrap();
        // Backdate ancient's timestamps directly — the public API only ever
        // stamps "now", so a real DB write is needed to simulate age.
        store
            .with_conn({
                let old = now - 200 * day;
                move |c| {
                    c.execute(
                        "UPDATE skill_usage SET last_used_at=?1, created_at=?1 WHERE name='ancient'",
                        rusqlite::params![old],
                    )
                    .map(|_| ())
                }
            })
            .await
            .unwrap();
        store.record_skill_use("fresh").await.unwrap();

        tick(&store).await;

        let ancient = store.get_skill_usage("ancient").await.unwrap().unwrap();
        assert_eq!(ancient.state, "archived");
        assert!(ancient.archived_at.is_some());
        let fresh = store.get_skill_usage("fresh").await.unwrap().unwrap();
        assert_eq!(fresh.state, "active", "recently-used skills are untouched");

        let runs = store.list_curator_runs(10).await.unwrap();
        assert_eq!(runs.len(), 1);
        assert_eq!(runs[0].status, "ok");
        assert_eq!(runs[0].transitioned, 1);
        assert!(runs[0].finished_at.is_some());
        let last_run = store.curator_last_run().await.unwrap();
        assert!(
            last_run.is_some_and(|t| t >= now),
            "curator_state.last_run_at must be stamped by this run, got {last_run:?}"
        );
    }

    #[tokio::test]
    async fn tick_never_archives_a_pinned_skill_even_when_ancient() {
        let tmp = tempfile::NamedTempFile::new().unwrap();
        let store = Arc::new(Store::open(tmp.path()).await.unwrap());
        let now = crate::paths::now_ms();
        let day = DAY_MS;
        store.record_skill_use("beloved").await.unwrap();
        store.set_skill_pinned("beloved", true).await.unwrap();
        store
            .with_conn({
                let old = now - 200 * day;
                move |c| {
                    c.execute(
                        "UPDATE skill_usage SET last_used_at=?1, created_at=?1 WHERE name='beloved'",
                        rusqlite::params![old],
                    )
                    .map(|_| ())
                }
            })
            .await
            .unwrap();

        tick(&store).await;

        let beloved = store.get_skill_usage("beloved").await.unwrap().unwrap();
        assert_eq!(beloved.state, "active", "a pinned skill is never archived");
    }
}
