use std::path::Path;
use std::sync::Arc;

use async_trait::async_trait;
use ryuzi_core::domain::{
    ApprovalDecision, ApprovalKind, ApprovalResponse, ApprovalScope, NewMessage,
};
use ryuzi_core::harness::{Harness, HarnessFactory, HarnessSession, SessionCtx, TurnPrompt};
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
    async fn send_prompt(&self, _prompt: TurnPrompt) -> anyhow::Result<()> {
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
) -> ryuzi_runner::dispatch::Deps {
    let o = out.clone();
    let e = errs.clone();
    ryuzi_runner::dispatch::Deps {
        db_path: db.to_path_buf(),
        out: Box::new(move |s| o.lock().unwrap().push(s.to_string())),
        err: Box::new(move |s| e.lock().unwrap().push(s.to_string())),
        prompt: Box::new(|_| "n".into()),
        detect_git: || ryuzi_runner::detect::Detected {
            found: true,
            version: None,
        },
        build_registries: Box::new(|| {
            let mut r = Registries::new();
            // new projects are always created with the native harness
            r.harness = Arc::new(FakeFactory);
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
    let code = ryuzi_runner::dispatch::run_cli(args, &mut deps);

    let out = out.lock().unwrap();
    assert!(out.iter().any(|l| l == "all done"), "stdout: {out:?}");
    assert_eq!(out.last().map(String::as_str), Some("✓ done"));
    assert_eq!(code, 0);
}

#[test]
#[serial]
fn run_rejects_removed_harness_flag_as_unknown_argument() {
    let tmp = tempfile::tempdir().unwrap();
    let out = Arc::new(std::sync::Mutex::new(Vec::new()));
    let errs = Arc::new(std::sync::Mutex::new(Vec::new()));
    let mut deps = deps_with_fake(&tmp.path().join("ryuzi.sqlite"), out.clone(), errs.clone());
    let code = ryuzi_runner::dispatch::run_cli(
        vec![
            "run".into(),
            "--harness".into(),
            "native".into(),
            "--dir".into(),
            "/tmp".into(),
            "--prompt".into(),
            "x".into(),
        ],
        &mut deps,
    );
    assert_eq!(code, 1);
    // clap's standard unknown-argument error, verbatim from e.to_string()
    let errs = errs.lock().unwrap();
    assert!(
        errs.iter().any(|l| l.contains("--harness")),
        "clap must reject --harness as an unknown argument: {errs:?}"
    );
}

#[test]
#[serial]
fn run_usage_and_mode_validation() {
    let tmp = tempfile::tempdir().unwrap();
    let out = Arc::new(std::sync::Mutex::new(Vec::new()));
    let errs = Arc::new(std::sync::Mutex::new(Vec::new()));

    let mut deps = deps_with_fake(&tmp.path().join("ryuzi.sqlite"), out.clone(), errs.clone());
    let code =
        ryuzi_runner::dispatch::run_cli(vec!["run".into(), "--prompt".into(), "x".into()], &mut deps);
    assert_eq!(code, 1);
    assert_eq!(
        errs.lock().unwrap().last().map(String::as_str),
        Some(
            "usage: ryuzi run --dir <git-repo> --prompt <text> [--model x] [--effort y] [--mode m]"
        )
    );

    let mut deps = deps_with_fake(&tmp.path().join("ryuzi.sqlite"), out.clone(), errs.clone());
    let code = ryuzi_runner::dispatch::run_cli(
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

// A harness whose turn never completes: send_prompt awaits a future that never
// resolves, so ControlPlane never emits Result/Error. The run loop must still
// exit once the session row leaves Running (poll fallback).
struct BlockingFakeSession;

#[async_trait]
impl HarnessSession for BlockingFakeSession {
    async fn send_prompt(&self, _prompt: TurnPrompt) -> anyhow::Result<()> {
        std::future::pending::<()>().await;
        unreachable!()
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

struct BlockingFakeHarness;
#[async_trait]
impl Harness for BlockingFakeHarness {
    async fn start_session(&self, _ctx: SessionCtx) -> anyhow::Result<Box<dyn HarnessSession>> {
        Ok(Box::new(BlockingFakeSession))
    }
}

struct BlockingFactory;
impl HarnessFactory for BlockingFactory {
    fn create(&self) -> anyhow::Result<Arc<dyn Harness>> {
        Ok(Arc::new(BlockingFakeHarness))
    }
}

#[test]
#[serial]
fn run_exits_when_session_demoted_even_without_terminal_event() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path().join("repo");
    std::fs::create_dir(&repo).unwrap();
    git_repo_fixture(&repo);
    std::env::set_var("XDG_DATA_HOME", tmp.path().join("data"));
    std::env::set_var("HOME", tmp.path());

    let db = tmp.path().join("ryuzi.sqlite");
    let out = Arc::new(std::sync::Mutex::new(Vec::new()));
    let errs = Arc::new(std::sync::Mutex::new(Vec::new()));
    let mut deps = deps_with_fake(&db, out.clone(), errs.clone());
    deps.build_registries = Box::new(|| {
        let mut r = Registries::new();
        r.harness = Arc::new(BlockingFactory);
        Ok(r)
    });

    // External demotion: a second Store handle flips the (only) session to Idle
    // after 1s, simulating a lost terminal event.
    let db2 = db.clone();
    let demoter = std::thread::spawn(move || {
        std::thread::sleep(std::time::Duration::from_secs(1));
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        rt.block_on(async {
            let store = ryuzi_core::Store::open(&db2).await.unwrap();
            let sessions = store.list_sessions(None).await.unwrap();
            store
                .update_status(
                    &sessions[0].session_pk,
                    ryuzi_core::domain::SessionStatus::Idle,
                    None,
                )
                .await
                .unwrap();
        });
    });

    let start = std::time::Instant::now();
    let args: Vec<String> = ["run", "--dir", repo.to_str().unwrap(), "--prompt", "hi"]
        .iter()
        .map(|s| s.to_string())
        .collect();
    let code = ryuzi_runner::dispatch::run_cli(args, &mut deps);
    demoter.join().unwrap();

    assert_eq!(code, 0);
    assert!(
        start.elapsed() < std::time::Duration::from_secs(10),
        "run loop must not hang"
    );
    assert_eq!(
        out.lock().unwrap().last().map(String::as_str),
        Some("✓ done")
    );
}

// --- Task 7: Plan/Question CLI prompts ---
//
// `ApprovalFakeSession` raises exactly one mid-turn `ApprovalRequested` (kind
// and input supplied by the test), then — the same "hub receiver" pattern
// `crates/core/src/approval.rs`'s own unit tests use — blocks on the shared
// `ApprovalHub`'s oneshot receiver until `run_cmd`'s event loop resolves it,
// capturing the exact `ApprovalResponse` the CLI produced into `resolved` for
// the test to assert on. `send_prompt` runs on ControlPlane's spawned turn
// task (control/lifecycle.rs's `spawn_prompt`), so blocking here on the CLI's
// own resolution does not deadlock the run loop.
struct ApprovalFakeSession {
    ctx_events: tokio::sync::broadcast::Sender<CoreEvent>,
    store: Arc<ryuzi_core::Store>,
    session_pk: String,
    approvals: Arc<ryuzi_core::approval::ApprovalHub>,
    approval_kind: ApprovalKind,
    input: serde_json::Value,
    resolved: Arc<std::sync::Mutex<Option<ApprovalResponse>>>,
}

#[async_trait]
impl HarnessSession for ApprovalFakeSession {
    async fn send_prompt(&self, _prompt: TurnPrompt) -> anyhow::Result<()> {
        let request_id = format!("test-approval-{}", self.session_pk);
        let rx = self.approvals.register(request_id.clone());
        let _ = self.ctx_events.send(CoreEvent::ApprovalRequested {
            session_pk: self.session_pk.clone(),
            request_id,
            tool: "exitplanmode".into(),
            summary: "review the plan".into(),
            approval_kind: self.approval_kind,
            input: self.input.clone(),
        });
        let response = rx.await.expect("run_cmd must resolve the parked approval");
        *self.resolved.lock().unwrap() = Some(response);

        // Same tail as FakeSession: finish the turn normally so ControlPlane's
        // spawn_prompt sees Ok(()) and emits Result (→ run_cmd prints "✓ done").
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

struct ApprovalFakeHarness {
    approval_kind: ApprovalKind,
    input: serde_json::Value,
    resolved: Arc<std::sync::Mutex<Option<ApprovalResponse>>>,
}

#[async_trait]
impl Harness for ApprovalFakeHarness {
    async fn start_session(&self, ctx: SessionCtx) -> anyhow::Result<Box<dyn HarnessSession>> {
        Ok(Box::new(ApprovalFakeSession {
            ctx_events: ctx.events.clone(),
            store: ctx.store.clone(),
            session_pk: ctx.session_pk.clone(),
            approvals: ctx.approvals.clone(),
            approval_kind: self.approval_kind,
            input: self.input.clone(),
            resolved: self.resolved.clone(),
        }))
    }
}

struct ApprovalFakeFactory {
    approval_kind: ApprovalKind,
    input: serde_json::Value,
    resolved: Arc<std::sync::Mutex<Option<ApprovalResponse>>>,
}

impl HarnessFactory for ApprovalFakeFactory {
    fn create(&self) -> anyhow::Result<Arc<dyn Harness>> {
        Ok(Arc::new(ApprovalFakeHarness {
            approval_kind: self.approval_kind,
            input: self.input.clone(),
            resolved: self.resolved.clone(),
        }))
    }
}

/// Like `deps_with_fake`, but the harness raises a Plan/Question approval
/// mid-turn and `prompts` scripts the CLI's replies in order — one string per
/// `deps.prompt` call. Plan-reject and Question flows need more than one.
#[allow(clippy::too_many_arguments)]
fn deps_with_approval_fake(
    db: &Path,
    out: Arc<std::sync::Mutex<Vec<String>>>,
    errs: Arc<std::sync::Mutex<Vec<String>>>,
    approval_kind: ApprovalKind,
    input: serde_json::Value,
    prompts: Vec<&'static str>,
    resolved: Arc<std::sync::Mutex<Option<ApprovalResponse>>>,
) -> ryuzi_runner::dispatch::Deps {
    let o = out.clone();
    let e = errs.clone();
    let mut prompts = prompts.into_iter();
    ryuzi_runner::dispatch::Deps {
        db_path: db.to_path_buf(),
        out: Box::new(move |s| o.lock().unwrap().push(s.to_string())),
        err: Box::new(move |s| e.lock().unwrap().push(s.to_string())),
        prompt: Box::new(move |_| prompts.next().unwrap_or_default().to_string()),
        detect_git: || ryuzi_runner::detect::Detected {
            found: true,
            version: None,
        },
        build_registries: Box::new(move || {
            let mut r = Registries::new();
            r.harness = Arc::new(ApprovalFakeFactory {
                approval_kind,
                input: input.clone(),
                resolved: resolved.clone(),
            });
            Ok(r)
        }),
    }
}

#[test]
#[serial]
fn plan_review_approve_sends_accept_edits_payload() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path().join("repo");
    std::fs::create_dir(&repo).unwrap();
    git_repo_fixture(&repo);
    std::env::set_var("XDG_DATA_HOME", tmp.path().join("data"));
    std::env::set_var("HOME", tmp.path());

    let out = Arc::new(std::sync::Mutex::new(Vec::new()));
    let errs = Arc::new(std::sync::Mutex::new(Vec::new()));
    let resolved = Arc::new(std::sync::Mutex::new(None));
    let mut deps = deps_with_approval_fake(
        &tmp.path().join("ryuzi.sqlite"),
        out.clone(),
        errs.clone(),
        ApprovalKind::Plan,
        serde_json::json!({"plan": "step 1"}),
        vec!["a"],
        resolved.clone(),
    );

    let args: Vec<String> = ["run", "--dir", repo.to_str().unwrap(), "--prompt", "hi"]
        .iter()
        .map(|s| s.to_string())
        .collect();
    let code = ryuzi_runner::dispatch::run_cli(args, &mut deps);

    assert_eq!(code, 0, "errs: {:?}", errs.lock().unwrap());
    let response = resolved
        .lock()
        .unwrap()
        .clone()
        .expect("the plan approval must resolve");
    assert_eq!(response.decision, ApprovalDecision::AllowOnce);
    assert_eq!(
        response.payload,
        Some(serde_json::json!({"mode": "acceptEdits"}))
    );
    assert!(
        out.lock().unwrap().iter().any(|l| l == "step 1"),
        "the proposed plan text should be printed before the prompt"
    );
}

#[test]
#[serial]
fn plan_review_reject_sends_feedback_payload() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path().join("repo");
    std::fs::create_dir(&repo).unwrap();
    git_repo_fixture(&repo);
    std::env::set_var("XDG_DATA_HOME", tmp.path().join("data"));
    std::env::set_var("HOME", tmp.path());

    let out = Arc::new(std::sync::Mutex::new(Vec::new()));
    let errs = Arc::new(std::sync::Mutex::new(Vec::new()));
    let resolved = Arc::new(std::sync::Mutex::new(None));
    let mut deps = deps_with_approval_fake(
        &tmp.path().join("ryuzi.sqlite"),
        out.clone(),
        errs.clone(),
        ApprovalKind::Plan,
        serde_json::json!({"plan": "step 1"}),
        vec!["r", "needs more tests"],
        resolved.clone(),
    );

    let args: Vec<String> = ["run", "--dir", repo.to_str().unwrap(), "--prompt", "hi"]
        .iter()
        .map(|s| s.to_string())
        .collect();
    let code = ryuzi_runner::dispatch::run_cli(args, &mut deps);

    assert_eq!(code, 0, "errs: {:?}", errs.lock().unwrap());
    let response = resolved
        .lock()
        .unwrap()
        .clone()
        .expect("the plan approval must resolve");
    assert_eq!(response.decision, ApprovalDecision::RejectOnce);
    assert_eq!(
        response.payload,
        Some(serde_json::json!({"feedback": "needs more tests"}))
    );
}

#[test]
#[serial]
fn question_numeric_answer_maps_to_option_label() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path().join("repo");
    std::fs::create_dir(&repo).unwrap();
    git_repo_fixture(&repo);
    std::env::set_var("XDG_DATA_HOME", tmp.path().join("data"));
    std::env::set_var("HOME", tmp.path());

    let out = Arc::new(std::sync::Mutex::new(Vec::new()));
    let errs = Arc::new(std::sync::Mutex::new(Vec::new()));
    let resolved = Arc::new(std::sync::Mutex::new(None));
    let input = serde_json::json!({"questions": [{
        "question": "Which DB?",
        "header": "db",
        "options": [{"label": "SQLite"}, {"label": "Postgres"}]
    }]});
    let mut deps = deps_with_approval_fake(
        &tmp.path().join("ryuzi.sqlite"),
        out.clone(),
        errs.clone(),
        ApprovalKind::Question,
        input,
        vec!["1"],
        resolved.clone(),
    );

    let args: Vec<String> = ["run", "--dir", repo.to_str().unwrap(), "--prompt", "hi"]
        .iter()
        .map(|s| s.to_string())
        .collect();
    let code = ryuzi_runner::dispatch::run_cli(args, &mut deps);

    assert_eq!(code, 0, "errs: {:?}", errs.lock().unwrap());
    let response = resolved
        .lock()
        .unwrap()
        .clone()
        .expect("the question approval must resolve");
    assert_eq!(response.decision, ApprovalDecision::AllowOnce);
    assert_eq!(
        response.payload,
        Some(serde_json::json!({"answers": {"Which DB?": ["SQLite"]}}))
    );
}

#[test]
#[serial]
fn tool_approval_scripted_capital_n_persists_reject_always_project() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path().join("repo");
    std::fs::create_dir(&repo).unwrap();
    git_repo_fixture(&repo);
    std::env::set_var("XDG_DATA_HOME", tmp.path().join("data"));
    std::env::set_var("HOME", tmp.path());

    let out = Arc::new(std::sync::Mutex::new(Vec::new()));
    let errs = Arc::new(std::sync::Mutex::new(Vec::new()));
    let resolved = Arc::new(std::sync::Mutex::new(None));
    let mut deps = deps_with_approval_fake(
        &tmp.path().join("ryuzi.sqlite"),
        out.clone(),
        errs.clone(),
        ApprovalKind::Tool,
        serde_json::json!({}),
        vec!["N"],
        resolved.clone(),
    );

    let args: Vec<String> = ["run", "--dir", repo.to_str().unwrap(), "--prompt", "hi"]
        .iter()
        .map(|s| s.to_string())
        .collect();
    let code = ryuzi_runner::dispatch::run_cli(args, &mut deps);

    assert_eq!(code, 0, "errs: {:?}", errs.lock().unwrap());
    let response = resolved
        .lock()
        .unwrap()
        .clone()
        .expect("the tool approval must resolve");
    assert_eq!(response.decision, ApprovalDecision::RejectAlways);
    assert_eq!(response.scope, Some(ApprovalScope::Project));
}

#[test]
#[serial]
fn tool_approval_scripted_lowercase_s_allows_always_session() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path().join("repo");
    std::fs::create_dir(&repo).unwrap();
    git_repo_fixture(&repo);
    std::env::set_var("XDG_DATA_HOME", tmp.path().join("data"));
    std::env::set_var("HOME", tmp.path());

    let out = Arc::new(std::sync::Mutex::new(Vec::new()));
    let errs = Arc::new(std::sync::Mutex::new(Vec::new()));
    let resolved = Arc::new(std::sync::Mutex::new(None));
    let mut deps = deps_with_approval_fake(
        &tmp.path().join("ryuzi.sqlite"),
        out.clone(),
        errs.clone(),
        ApprovalKind::Tool,
        serde_json::json!({}),
        vec!["s"],
        resolved.clone(),
    );

    let args: Vec<String> = ["run", "--dir", repo.to_str().unwrap(), "--prompt", "hi"]
        .iter()
        .map(|s| s.to_string())
        .collect();
    let code = ryuzi_runner::dispatch::run_cli(args, &mut deps);

    assert_eq!(code, 0, "errs: {:?}", errs.lock().unwrap());
    let response = resolved
        .lock()
        .unwrap()
        .clone()
        .expect("the tool approval must resolve");
    assert_eq!(response.decision, ApprovalDecision::AllowAlways);
    assert_eq!(response.scope, Some(ApprovalScope::Session));
}

#[test]
#[serial]
fn question_free_text_answer_becomes_single_element_other() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path().join("repo");
    std::fs::create_dir(&repo).unwrap();
    git_repo_fixture(&repo);
    std::env::set_var("XDG_DATA_HOME", tmp.path().join("data"));
    std::env::set_var("HOME", tmp.path());

    let out = Arc::new(std::sync::Mutex::new(Vec::new()));
    let errs = Arc::new(std::sync::Mutex::new(Vec::new()));
    let resolved = Arc::new(std::sync::Mutex::new(None));
    let input = serde_json::json!({"questions": [{
        "question": "Which DB?",
        "header": "db",
        "options": [{"label": "SQLite"}, {"label": "Postgres"}]
    }]});
    let mut deps = deps_with_approval_fake(
        &tmp.path().join("ryuzi.sqlite"),
        out.clone(),
        errs.clone(),
        ApprovalKind::Question,
        input,
        vec!["my own answer"],
        resolved.clone(),
    );

    let args: Vec<String> = ["run", "--dir", repo.to_str().unwrap(), "--prompt", "hi"]
        .iter()
        .map(|s| s.to_string())
        .collect();
    let code = ryuzi_runner::dispatch::run_cli(args, &mut deps);

    assert_eq!(code, 0, "errs: {:?}", errs.lock().unwrap());
    let response = resolved
        .lock()
        .unwrap()
        .clone()
        .expect("the question approval must resolve");
    assert_eq!(response.decision, ApprovalDecision::AllowOnce);
    assert_eq!(
        response.payload,
        Some(serde_json::json!({"answers": {"Which DB?": ["my own answer"]}}))
    );
}

#[test]
#[serial]
fn question_zero_is_not_a_valid_option() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path().join("repo");
    std::fs::create_dir(&repo).unwrap();
    git_repo_fixture(&repo);
    std::env::set_var("XDG_DATA_HOME", tmp.path().join("data"));
    std::env::set_var("HOME", tmp.path());

    let out = Arc::new(std::sync::Mutex::new(Vec::new()));
    let errs = Arc::new(std::sync::Mutex::new(Vec::new()));
    let resolved = Arc::new(std::sync::Mutex::new(None));
    let input = serde_json::json!({"questions": [{
        "question": "Which DB?",
        "header": "db",
        "options": [{"label": "SQLite"}, {"label": "Postgres"}]
    }]});
    let mut deps = deps_with_approval_fake(
        &tmp.path().join("ryuzi.sqlite"),
        out.clone(),
        errs.clone(),
        ApprovalKind::Question,
        input,
        vec!["0"],
        resolved.clone(),
    );

    let args: Vec<String> = ["run", "--dir", repo.to_str().unwrap(), "--prompt", "hi"]
        .iter()
        .map(|s| s.to_string())
        .collect();
    let code = ryuzi_runner::dispatch::run_cli(args, &mut deps);

    assert_eq!(code, 0, "errs: {:?}", errs.lock().unwrap());
    let response = resolved
        .lock()
        .unwrap()
        .clone()
        .expect("the question approval must resolve");
    assert_eq!(response.decision, ApprovalDecision::AllowOnce);
    assert_eq!(
        response.payload,
        Some(serde_json::json!({"answers": {"Which DB?": ["0"]}}))
    );
}

#[test]
#[serial]
fn question_out_of_range_number_falls_back_to_free_text() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path().join("repo");
    std::fs::create_dir(&repo).unwrap();
    git_repo_fixture(&repo);
    std::env::set_var("XDG_DATA_HOME", tmp.path().join("data"));
    std::env::set_var("HOME", tmp.path());

    let out = Arc::new(std::sync::Mutex::new(Vec::new()));
    let errs = Arc::new(std::sync::Mutex::new(Vec::new()));
    let resolved = Arc::new(std::sync::Mutex::new(None));
    let input = serde_json::json!({"questions": [{
        "question": "Which DB?",
        "header": "db",
        "options": [{"label": "SQLite"}, {"label": "Postgres"}]
    }]});
    let mut deps = deps_with_approval_fake(
        &tmp.path().join("ryuzi.sqlite"),
        out.clone(),
        errs.clone(),
        ApprovalKind::Question,
        input,
        vec!["9"],
        resolved.clone(),
    );

    let args: Vec<String> = ["run", "--dir", repo.to_str().unwrap(), "--prompt", "hi"]
        .iter()
        .map(|s| s.to_string())
        .collect();
    let code = ryuzi_runner::dispatch::run_cli(args, &mut deps);

    assert_eq!(code, 0, "errs: {:?}", errs.lock().unwrap());
    let response = resolved
        .lock()
        .unwrap()
        .clone()
        .expect("the question approval must resolve");
    assert_eq!(response.decision, ApprovalDecision::AllowOnce);
    assert_eq!(
        response.payload,
        Some(serde_json::json!({"answers": {"Which DB?": ["9"]}}))
    );
}

#[test]
#[serial]
fn question_mixed_valid_and_junk_falls_back_to_free_text() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path().join("repo");
    std::fs::create_dir(&repo).unwrap();
    git_repo_fixture(&repo);
    std::env::set_var("XDG_DATA_HOME", tmp.path().join("data"));
    std::env::set_var("HOME", tmp.path());

    let out = Arc::new(std::sync::Mutex::new(Vec::new()));
    let errs = Arc::new(std::sync::Mutex::new(Vec::new()));
    let resolved = Arc::new(std::sync::Mutex::new(None));
    let input = serde_json::json!({"questions": [{
        "question": "Which DB?",
        "header": "db",
        "options": [{"label": "SQLite"}, {"label": "Postgres"}]
    }]});
    let mut deps = deps_with_approval_fake(
        &tmp.path().join("ryuzi.sqlite"),
        out.clone(),
        errs.clone(),
        ApprovalKind::Question,
        input,
        vec!["1,junk"],
        resolved.clone(),
    );

    let args: Vec<String> = ["run", "--dir", repo.to_str().unwrap(), "--prompt", "hi"]
        .iter()
        .map(|s| s.to_string())
        .collect();
    let code = ryuzi_runner::dispatch::run_cli(args, &mut deps);

    assert_eq!(code, 0, "errs: {:?}", errs.lock().unwrap());
    let response = resolved
        .lock()
        .unwrap()
        .clone()
        .expect("the question approval must resolve");
    assert_eq!(response.decision, ApprovalDecision::AllowOnce);
    assert_eq!(
        response.payload,
        Some(serde_json::json!({"answers": {"Which DB?": ["1,junk"]}}))
    );
}

#[test]
#[serial]
fn question_multiple_numbers_map_to_labels() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path().join("repo");
    std::fs::create_dir(&repo).unwrap();
    git_repo_fixture(&repo);
    std::env::set_var("XDG_DATA_HOME", tmp.path().join("data"));
    std::env::set_var("HOME", tmp.path());

    let out = Arc::new(std::sync::Mutex::new(Vec::new()));
    let errs = Arc::new(std::sync::Mutex::new(Vec::new()));
    let resolved = Arc::new(std::sync::Mutex::new(None));
    let input = serde_json::json!({"questions": [{
        "question": "Which DBs?",
        "header": "db",
        "multiSelect": true,
        "options": [{"label": "SQLite"}, {"label": "Postgres"}]
    }]});
    let mut deps = deps_with_approval_fake(
        &tmp.path().join("ryuzi.sqlite"),
        out.clone(),
        errs.clone(),
        ApprovalKind::Question,
        input,
        vec!["1,2"],
        resolved.clone(),
    );

    let args: Vec<String> = ["run", "--dir", repo.to_str().unwrap(), "--prompt", "hi"]
        .iter()
        .map(|s| s.to_string())
        .collect();
    let code = ryuzi_runner::dispatch::run_cli(args, &mut deps);

    assert_eq!(code, 0, "errs: {:?}", errs.lock().unwrap());
    let response = resolved
        .lock()
        .unwrap()
        .clone()
        .expect("the question approval must resolve");
    assert_eq!(response.decision, ApprovalDecision::AllowOnce);
    assert_eq!(
        response.payload,
        Some(serde_json::json!({"answers": {"Which DBs?": ["SQLite", "Postgres"]}}))
    );
}
