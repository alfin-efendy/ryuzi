use std::path::Path;
use std::sync::Arc;

use async_trait::async_trait;
use ryuzi_core::domain::NewMessage;
use ryuzi_core::harness::{Harness, HarnessFactory, HarnessSession, SessionCtx};
use ryuzi_core::integration::Integration;
use ryuzi_core::{CoreEvent, Registries};
use serial_test::serial;

// Mirrors the FakeSession pattern in crates/core/src/control.rs:343 — but lives
// here because core's test fixtures are private. ControlPlane itself emits
// Result/Error after send_prompt returns (control.rs:252-268), so the fake only
// persists+broadcasts the assistant text.
struct FakeSession {
    ctx_events: tokio::sync::broadcast::Sender<CoreEvent>,
    store: Arc<ryuzi_core::Store>,
    session_pk: String,
}

#[async_trait]
impl HarnessSession for FakeSession {
    async fn send_prompt(&self, _prompt: String) -> anyhow::Result<()> {
        let seq = self
            .store
            .insert_message(NewMessage::block(
                &self.session_pk,
                "assistant",
                "text",
                serde_json::json!({ "text": "all done" }),
            ))
            .await?;
        let _ = self.ctx_events.send(CoreEvent::Message {
            session_pk: self.session_pk.clone(),
            seq,
            role: "assistant".into(),
            block_type: "text".into(),
            payload: serde_json::json!({ "text": "all done" }),
            tool_call_id: None,
            status: None,
            tool_kind: None,
        });
        Ok(())
    }
    async fn cancel(&self) -> anyhow::Result<()> {
        Ok(())
    }
    async fn end(&self) -> anyhow::Result<()> {
        Ok(())
    }
    fn agent_session_id(&self) -> Option<String> {
        None
    }
}

struct FakeHarness;
#[async_trait]
impl Harness for FakeHarness {
    async fn start_session(&self, ctx: SessionCtx) -> anyhow::Result<Box<dyn HarnessSession>> {
        Ok(Box::new(FakeSession {
            ctx_events: ctx.events.clone(),
            store: ctx.store.clone(),
            session_pk: ctx.session_pk.clone(),
        }))
    }
}

struct FakeFactory;
impl HarnessFactory for FakeFactory {
    fn create(&self) -> anyhow::Result<Arc<dyn Harness>> {
        Ok(Arc::new(FakeHarness))
    }
}

struct FakeIntegration;
impl Integration for FakeIntegration {
    fn id(&self) -> &str {
        "claude-code"
    } // registry id must match connect_project's hardcoded harness
    fn harness(&self) -> Option<Arc<dyn HarnessFactory>> {
        Some(Arc::new(FakeFactory))
    }
}

fn git_repo_fixture(dir: &Path) {
    let run = |args: &[&str]| {
        assert!(std::process::Command::new("git")
            .args(args)
            .current_dir(dir)
            .status()
            .unwrap()
            .success());
    };
    run(&["init", "-q"]);
    std::fs::write(dir.join("README.md"), "hi").unwrap();
    run(&["add", "."]);
    run(&[
        "-c",
        "user.email=t@t",
        "-c",
        "user.name=t",
        "commit",
        "-qm",
        "init",
    ]);
}

// Worktrees land under state_dir(); the caller redirects that via HOME/XDG_DATA_HOME.
fn deps_with_fake(
    db: &Path,
    out: Arc<std::sync::Mutex<Vec<String>>>,
    errs: Arc<std::sync::Mutex<Vec<String>>>,
) -> ryuzi_cli::dispatch::Deps {
    let o = out.clone();
    let e = errs.clone();
    ryuzi_cli::dispatch::Deps {
        db_path: db.to_path_buf(),
        out: Box::new(move |s| o.lock().unwrap().push(s.to_string())),
        err: Box::new(move |s| e.lock().unwrap().push(s.to_string())),
        prompt: Box::new(|_| "n".into()),
        detect_git: || ryuzi_cli::detect::Detected {
            found: true,
            version: None,
        },
        detect_claude: || ryuzi_cli::detect::Detected {
            found: true,
            version: None,
        },
        sidecar_status: Box::new(|| ryuzi_core::sidecar::SidecarStatus::CachedStandalone),
        build_registries: Box::new(|| {
            let mut r = Registries::new();
            r.install(&FakeIntegration);
            Ok(r)
        }),
    }
}

#[test]
#[serial]
fn run_happy_path_prints_text_and_done() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path().join("repo");
    std::fs::create_dir(&repo).unwrap();
    git_repo_fixture(&repo);
    // Redirect state_dir (worktrees) into the tempdir on both Linux and macOS.
    std::env::set_var("XDG_DATA_HOME", tmp.path().join("data"));
    std::env::set_var("HOME", tmp.path());

    let out = Arc::new(std::sync::Mutex::new(Vec::new()));
    let errs = Arc::new(std::sync::Mutex::new(Vec::new()));
    let mut deps = deps_with_fake(&tmp.path().join("ryuzi.sqlite"), out.clone(), errs.clone());

    let args: Vec<String> = ["run", "--dir", repo.to_str().unwrap(), "--prompt", "hello"]
        .iter()
        .map(|s| s.to_string())
        .collect();
    let code = ryuzi_cli::dispatch::run_cli(args, &mut deps);

    let out = out.lock().unwrap();
    assert!(out.iter().any(|l| l == "all done"), "stdout: {out:?}");
    assert_eq!(out.last().map(String::as_str), Some("✓ done"));
    assert_eq!(code, 0);
}

#[test]
#[serial]
fn run_usage_and_mode_validation() {
    let tmp = tempfile::tempdir().unwrap();
    let out = Arc::new(std::sync::Mutex::new(Vec::new()));
    let errs = Arc::new(std::sync::Mutex::new(Vec::new()));

    let mut deps = deps_with_fake(&tmp.path().join("ryuzi.sqlite"), out.clone(), errs.clone());
    let code =
        ryuzi_cli::dispatch::run_cli(vec!["run".into(), "--prompt".into(), "x".into()], &mut deps);
    assert_eq!(code, 1);
    assert_eq!(
        errs.lock().unwrap().last().map(String::as_str),
        Some(
            "usage: ryuzi run --dir <git-repo> --prompt <text> [--model x] [--effort y] [--mode m]"
        )
    );

    let mut deps = deps_with_fake(&tmp.path().join("ryuzi.sqlite"), out.clone(), errs.clone());
    let code = ryuzi_cli::dispatch::run_cli(
        vec![
            "run".into(),
            "--dir".into(),
            "/tmp".into(),
            "--prompt".into(),
            "x".into(),
            "--mode".into(),
            "bogus".into(),
        ],
        &mut deps,
    );
    assert_eq!(code, 1);
    assert_eq!(
        errs.lock().unwrap().last().map(String::as_str),
        Some("--mode must be one of: default, acceptEdits, bypassPermissions, plan")
    );
}
