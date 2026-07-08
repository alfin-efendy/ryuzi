//! `ryuzi orch` — submit, inspect, and manage orchestrated task graphs.
//!
//! Talks to the store directly (SQLite WAL handles cross-process access):
//! `submit` only queues rows — a running daemon host (Cockpit or `ryuzi
//! serve`) picks them up on its next dispatcher tick. Auto-decomposition
//! happens in-daemon; the CLI queues the plain single-task form.

use crate::dispatch::Deps;
use ryuzi_core::orch;

pub fn cmd_orch(args: &[String], deps: &mut Deps) -> u8 {
    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .expect("tokio runtime");
    rt.block_on(orch_inner(args, deps))
}

const USAGE: &str = "usage: ryuzi orch <submit|list|cancel|retry> ...\n  \
    submit --project <id> <goal...>   queue a goal (a running daemon executes it)\n  \
    list [<root-id>]                  show tasks (optionally one tree)\n  \
    cancel <id>                       cancel a task and its unfinished subtasks\n  \
    retry <id>                        re-queue a failed task";

async fn orch_inner(args: &[String], deps: &mut Deps) -> u8 {
    match args.first().map(String::as_str) {
        Some("submit") => {
            // Split off the one `--project <id>` flag pair by position; the
            // remaining tokens are the goal verbatim (which may legitimately
            // contain the literal string `--project`).
            let mut rest: Vec<String> = args[1..].to_vec();
            let project = match rest.iter().position(|a| a == "--project") {
                Some(i) if i + 1 < rest.len() => {
                    let p = rest[i + 1].clone();
                    rest.drain(i..=i + 1);
                    p
                }
                _ => {
                    (deps.err)("usage: ryuzi orch submit --project <id> <goal...>");
                    return 1;
                }
            };
            let goal = rest.join(" ");
            if goal.trim().is_empty() {
                (deps.err)("usage: ryuzi orch submit --project <id> <goal...>");
                return 1;
            }
            let store = match crate::db::open_store(deps).await {
                Ok(s) => s,
                Err(e) => {
                    (deps.err)(&format!("✗ {e}"));
                    return 1;
                }
            };
            match orch::queue_goal(&store, &project, &goal).await {
                Ok(root) => {
                    (deps.out)(&format!(
                        "queued {root} — a running daemon (Cockpit or `ryuzi serve`) will pick it up"
                    ));
                    0
                }
                Err(e) => {
                    (deps.err)(&format!("✗ {e}"));
                    1
                }
            }
        }
        Some("list") => {
            let store = match crate::db::open_store(deps).await {
                Ok(s) => s,
                Err(e) => {
                    (deps.err)(&format!("✗ {e}"));
                    return 1;
                }
            };
            let root = args.get(1).map(String::as_str);
            let tasks = match orch::list_tasks(&store, root).await {
                Ok(t) => t,
                Err(e) => {
                    (deps.err)(&format!("✗ {e}"));
                    return 1;
                }
            };
            if tasks.is_empty() {
                (deps.out)("(no orchestrated tasks)");
                return 0;
            }
            for t in tasks {
                let kind = if t.root_id.is_none() { "root" } else { " sub" };
                let mut line = format!("{}  {:<12} {}  {}", t.id, t.status, kind, t.title);
                if let Some(e) = &t.error {
                    line.push_str(&format!("  [{e}]"));
                }
                (deps.out)(&line);
            }
            0
        }
        Some("cancel") => {
            let Some(id) = args.get(1) else {
                (deps.err)("usage: ryuzi orch cancel <id>");
                return 1;
            };
            let store = match crate::db::open_store(deps).await {
                Ok(s) => s,
                Err(e) => {
                    (deps.err)(&format!("✗ {e}"));
                    return 1;
                }
            };
            match orch::cancel_tree(&store, id).await {
                Ok(0) => {
                    (deps.err)(&format!("nothing to cancel for {id}"));
                    1
                }
                Ok(n) => {
                    (deps.out)(&format!("cancelled {n} task(s)"));
                    0
                }
                Err(e) => {
                    (deps.err)(&format!("✗ {e}"));
                    1
                }
            }
        }
        Some("retry") => {
            let Some(id) = args.get(1) else {
                (deps.err)("usage: ryuzi orch retry <id>");
                return 1;
            };
            let store = match crate::db::open_store(deps).await {
                Ok(s) => s,
                Err(e) => {
                    (deps.err)(&format!("✗ {e}"));
                    return 1;
                }
            };
            match orch::retry_task(&store, id).await {
                Ok(true) => {
                    (deps.out)(&format!("requeued {id}"));
                    0
                }
                Ok(false) => {
                    (deps.err)(&format!("{id} is not a failed task"));
                    1
                }
                Err(e) => {
                    (deps.err)(&format!("✗ {e}"));
                    1
                }
            }
        }
        _ => {
            (deps.err)(USAGE);
            1
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::detect::Detected;
    use ryuzi_core::sidecar::SidecarStatus;

    /// Captured stdout/stderr lines from a fake `Deps`.
    type Sink = std::rc::Rc<std::cell::RefCell<Vec<String>>>;

    fn fake_deps(db: &std::path::Path) -> (crate::dispatch::Deps, Sink, Sink) {
        let out = std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));
        let err = std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));
        let (out2, err2) = (out.clone(), err.clone());
        let deps = crate::dispatch::Deps {
            db_path: db.to_path_buf(),
            out: Box::new(move |s| out2.borrow_mut().push(s.to_string())),
            err: Box::new(move |s| err2.borrow_mut().push(s.to_string())),
            prompt: Box::new(|_| String::new()),
            detect_git: || Detected {
                found: true,
                version: None,
            },
            detect_claude: || Detected {
                found: true,
                version: None,
            },
            sidecar_status: Box::new(|| SidecarStatus::Override),
            build_registries: Box::new(|| Ok(ryuzi_core::Registries::new())),
        };
        (deps, out, err)
    }

    #[tokio::main(flavor = "current_thread")]
    async fn seed_project(db: &std::path::Path) {
        let store = ryuzi_core::Store::open(db).await.unwrap();
        store
            .insert_project(ryuzi_core::Project {
                project_id: "p1".into(),
                name: "p1".into(),
                workdir: "/tmp/p1".into(),
                source: None,
                harness: "native".into(),
                model: None,
                effort: None,
                perm_mode: ryuzi_core::PermMode::Default,
                created_at: Some(0),
                is_git: false,
            })
            .await
            .unwrap();
    }

    fn args(list: &[&str]) -> Vec<String> {
        list.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn submit_queues_and_list_shows_the_tree() {
        let tmp = tempfile::NamedTempFile::new().unwrap();
        seed_project(tmp.path());
        let (mut deps, out, _err) = fake_deps(tmp.path());

        let code = cmd_orch(
            &args(&["submit", "--project", "p1", "fix", "the", "bug"]),
            &mut deps,
        );
        assert_eq!(code, 0);
        assert!(
            out.borrow()[0].starts_with("queued ot-"),
            "{:?}",
            out.borrow()
        );

        let code = cmd_orch(&args(&["list"]), &mut deps);
        assert_eq!(code, 0);
        let listing = out.borrow().join("\n");
        assert!(listing.contains("fix the bug"), "{listing}");
        assert!(listing.contains("root"), "{listing}");
        assert!(listing.contains("todo"), "{listing}");
    }

    #[test]
    fn submit_rejects_unknown_project_and_missing_goal() {
        let tmp = tempfile::NamedTempFile::new().unwrap();
        seed_project(tmp.path());
        let (mut deps, _out, err) = fake_deps(tmp.path());
        assert_eq!(
            cmd_orch(&args(&["submit", "--project", "nope", "x"]), &mut deps),
            1
        );
        assert!(err.borrow().last().unwrap().contains("unknown project"));
        assert_eq!(
            cmd_orch(&args(&["submit", "--project", "p1"]), &mut deps),
            1
        );
    }

    #[test]
    fn cancel_and_retry_roundtrip() {
        let tmp = tempfile::NamedTempFile::new().unwrap();
        seed_project(tmp.path());
        let (mut deps, out, err) = fake_deps(tmp.path());
        assert_eq!(
            cmd_orch(&args(&["submit", "--project", "p1", "goal"]), &mut deps),
            0
        );
        let root = out.borrow()[0]
            .split_whitespace()
            .nth(1)
            .unwrap()
            .to_string();

        assert_eq!(cmd_orch(&args(&["cancel", &root]), &mut deps), 0);
        assert!(out.borrow().last().unwrap().contains("cancelled 2"));
        // Cancelled tasks are not failed, so retry refuses.
        assert_eq!(cmd_orch(&args(&["cancel", &root]), &mut deps), 1);
        assert_eq!(cmd_orch(&args(&["retry", &root]), &mut deps), 1);
        assert!(err.borrow().last().unwrap().contains("not a failed task"));
    }

    #[test]
    fn unknown_subcommand_prints_usage() {
        let tmp = tempfile::NamedTempFile::new().unwrap();
        let (mut deps, _out, err) = fake_deps(tmp.path());
        assert_eq!(cmd_orch(&args(&["wat"]), &mut deps), 1);
        assert!(err.borrow()[0].contains("usage: ryuzi orch"));
    }
}
