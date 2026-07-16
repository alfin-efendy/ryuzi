//! The `ControlPlane`-backed implementation of the curated app-control
//! surface (spec §9.1). Holds a `Weak<ControlPlane>` so a session's
//! `ToolCtx` never keeps the plane alive through a background task or a
//! lingering `Arc` clone. Every mutating method reuses an EXISTING
//! `scheduler`/`orch`/`ControlPlane` function — no new engine logic — and
//! audits itself after the write succeeds; the three list/read methods do
//! not audit.

use crate::control::ControlPlane;
use crate::domain::WriteOrigin;
use crate::harness::native::tools::{AppControl, AppJobCreate, AppJobSummary, AppProjectSummary};
use async_trait::async_trait;
use std::sync::{Arc, Weak};

pub struct AppControlImpl {
    cp: Weak<ControlPlane>,
    origin: WriteOrigin,
}

impl AppControlImpl {
    pub fn new(cp: Weak<ControlPlane>, origin: WriteOrigin) -> Self {
        AppControlImpl { cp, origin }
    }

    /// Upgrade the weak handle, or fail with a message a tool can surface to
    /// the model verbatim — this only happens if the daemon is mid-shutdown
    /// and the plane has already been dropped out from under a live session.
    fn cp(&self) -> anyhow::Result<Arc<ControlPlane>> {
        self.cp
            .upgrade()
            .ok_or_else(|| anyhow::anyhow!("engine is shutting down"))
    }

    /// Best-effort: an audit failure must never fail the operation it
    /// records. `session_pk` is `None` — the facade isn't handed the calling
    /// session's id (Task 6's `ToolCtx.app` carries no such binding either),
    /// so every app-control audit row is session-less by construction.
    async fn audit(&self, tool: &str, action: &str) {
        if let Ok(cp) = self.cp() {
            let _ = cp
                .store()
                .record_audit(self.origin, None, tool, action, "allow")
                .await;
        }
    }
}

#[async_trait]
impl AppControl for AppControlImpl {
    fn origin(&self) -> WriteOrigin {
        self.origin
    }

    async fn list_jobs(&self) -> anyhow::Result<Vec<AppJobSummary>> {
        let cp = self.cp()?;
        let rows = crate::scheduler::list_jobs(cp.store()).await?;
        Ok(rows
            .into_iter()
            .map(|j| AppJobSummary {
                id: j.id,
                name: j.name,
                cron: j.cron,
                enabled: j.enabled,
            })
            .collect())
    }

    async fn create_job(&self, spec: AppJobCreate) -> anyhow::Result<String> {
        let cp = self.cp()?;
        // `spec.schedule` is natural language OR a raw cron expression — try
        // natural first (its own parser only accepts phrases it recognizes),
        // falling back to validating the raw text as cron via a next-fire
        // probe, exactly like `scheduler_api::resolve_cron`'s cron branch.
        let (mode, cron) = match crate::scheduler::natural_to_cron(&spec.schedule) {
            Some(cron) => ("natural", cron),
            None => {
                if crate::scheduler::next_run_after(&spec.schedule, crate::paths::now_ms())
                    .is_none()
                {
                    anyhow::bail!(
                        "could not parse schedule {:?} as natural language or cron",
                        spec.schedule
                    );
                }
                ("cron", spec.schedule.clone())
            }
        };
        let id = format!("job-{}", &crate::paths::new_id()[..8]);
        let job = crate::scheduler::JobRow {
            id: id.clone(),
            name: spec.name,
            cron,
            mode: mode.into(),
            natural_text: if mode == "natural" {
                spec.schedule
            } else {
                String::new()
            },
            project_id: spec.project_id.unwrap_or_default(),
            branch: String::new(),
            gateway: String::new(),
            enabled: true,
            prompt: spec.prompt,
            notify_success: false,
            notify_fail: true,
            pre_check: String::new(),
            model_override: spec.model_override,
        };
        crate::scheduler::upsert_job(cp.store(), job).await?;
        self.audit("app_jobs", "create").await;
        Ok(id)
    }

    async fn set_job_enabled(&self, id: &str, enabled: bool) -> anyhow::Result<bool> {
        let cp = self.cp()?;
        let Some(mut job) = crate::scheduler::get_job(cp.store(), id).await? else {
            return Ok(false);
        };
        job.enabled = enabled;
        crate::scheduler::upsert_job(cp.store(), job).await?;
        self.audit("app_jobs", if enabled { "resume" } else { "pause" })
            .await;
        Ok(true)
    }

    async fn run_job_now(&self, id: &str) -> anyhow::Result<String> {
        let cp = self.cp()?;
        let Some(job) = crate::scheduler::get_job(cp.store(), id).await? else {
            anyhow::bail!("no such job {id}");
        };
        if crate::scheduler::has_running_run(cp.store(), id).await? {
            anyhow::bail!("job {id} is already running");
        }
        let run_id = crate::scheduler::execute_job(&cp, &job).await?;
        self.audit("app_jobs", "run").await;
        Ok(run_id)
    }

    async fn list_projects(&self) -> anyhow::Result<Vec<AppProjectSummary>> {
        let cp = self.cp()?;
        Ok(cp
            .list_projects()
            .await?
            .into_iter()
            .map(|p| AppProjectSummary {
                id: p.project_id,
                name: p.name,
            })
            .collect())
    }

    async fn create_chat_session(&self, title: Option<String>) -> anyhow::Result<String> {
        let cp = self.cp()?;
        // No empty-session primitive exists yet: `start_chat_session` requires
        // a `TurnPrompt` and immediately drives one real chat-agent turn in
        // the background.
        // Pragmatic choice: use the title (or a generic placeholder) as that
        // turn's prompt, so "create a chat" reads as "start a chat with this
        // opener" rather than blocking the whole facade on a new primitive.
        // A true empty-home-chat start is a known Phase 5/6 fast-follow.
        let text = title.unwrap_or_else(|| "New chat".to_string());
        let prompt = crate::harness::TurnPrompt::text(text.clone(), text);
        let session = cp.start_chat_session(prompt, "agent", &[]).await?;
        self.audit("app_projects", "create_chat").await;
        Ok(session.session_pk)
    }

    async fn attach_project(&self, session_pk: &str, project_id: &str) -> anyhow::Result<()> {
        let cp = self.cp()?;
        cp.attach_project(session_pk, project_id).await?;
        self.audit("app_projects", "attach").await;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A `ControlPlane` over a fresh temp-file store, wired with the real
    /// (native) harness factory — unused by this test since `create_job`
    /// never starts a session, but keeping `Registries::default()` mirrors
    /// `ControlPlane::new(store, regs)` pattern rather than reaching into
    /// `crate::control::tests`'s private harness fakes, which aren't visible
    /// from a sibling module.
    async fn test_control_plane() -> Arc<ControlPlane> {
        let tmp = tempfile::NamedTempFile::new().unwrap();
        let store = std::sync::Arc::new(crate::store::Store::open(tmp.path()).await.unwrap());
        // Keep the backing file alive for the process; the test's OS temp
        // dir is cleaned up on process exit either way.
        std::mem::forget(tmp);
        {
            let persistence = crate::agents::bootstrap::AgentPersistence::temporary(
                std::sync::Arc::clone(&store),
            )
            .await
            .unwrap();
            ControlPlane::new(store, crate::plugins::Registries::new(), persistence).await
        }
    }

    #[tokio::test]
    async fn create_job_writes_a_job_and_an_audit_row() {
        let cp = test_control_plane().await;
        let app = cp.build_app_control();
        let id = app
            .create_job(AppJobCreate {
                name: "nightly".into(),
                schedule: "every day at 9am".into(),
                prompt: "summarize the day".into(),
                project_id: None,
                model_override: None,
            })
            .await
            .unwrap();
        assert!(!id.is_empty());
        // The job is persisted…
        let jobs = crate::scheduler::list_jobs(cp.store()).await.unwrap();
        assert!(jobs.iter().any(|j| j.id == id));
        // …and an audit row was written with agent origin.
        let audit = cp.store().list_audit(10).await.unwrap();
        assert_eq!(audit[0].tool, "app_jobs");
        assert_eq!(audit[0].action, "create");
        assert_eq!(audit[0].origin, WriteOrigin::Agent.as_str());
    }

    #[tokio::test]
    async fn list_reads_never_audit() {
        let cp = test_control_plane().await;
        let app = cp.build_app_control();
        app.list_jobs().await.unwrap();
        app.list_projects().await.unwrap();
        assert!(cp.store().list_audit(10).await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn dropped_control_plane_fails_closed_instead_of_panicking() {
        let cp = test_control_plane().await;
        let app = cp.build_app_control();
        drop(cp);
        let err = app.list_jobs().await.unwrap_err();
        assert!(err.to_string().contains("shutting down"));
    }
}
