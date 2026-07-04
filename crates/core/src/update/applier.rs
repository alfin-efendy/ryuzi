//! Spawn-before-swap applier state machine.
//! Safely stages a binary update: spawn a canary, monitor health, drain in-flight requests,
//! swap the live binary, and hand over to the new daemon, rolling back if anything fails.

use crate::update::handoff::{Handoff, HandoffPhase};
use crate::update::stage::StageResult;
use async_trait::async_trait;
use std::path::Path;

#[derive(Debug, Clone)]
pub struct ApplierCfg {
    pub version: String,
    pub drain_timeout_ms: u64,
    pub canary_timeout_ms: u64,
}

#[async_trait]
pub trait ApplierHost: Send + Sync {
    async fn stage(&self) -> StageResult;
    fn spawn_canary(&self, canary_path: &Path) -> anyhow::Result<i32>;
    fn read_handoff(&self) -> Option<Handoff>;
    fn write_handoff(&self, h: &Handoff);
    fn clear_handoff(&self);
    async fn drain(&self, timeout_ms: u64);
    fn backup(&self) -> anyhow::Result<()>;
    fn swap(&self) -> anyhow::Result<()>;
    fn restore(&self) -> anyhow::Result<()>;
    fn kill_canary(&self, pid: i32);
    async fn stop_gateways(&self);
    fn now(&self) -> i64;
    async fn sleep_ms(&self, ms: u64);
    fn log(&self, m: &str);
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ApplyOutcome {
    Promoted,
    Aborted,
    RolledBack,
}

async fn wait_for(
    cfg: &ApplierCfg,
    host: &dyn ApplierHost,
    pred: impl Fn(Option<&Handoff>) -> bool,
) -> Option<Handoff> {
    let deadline = host.now() + cfg.canary_timeout_ms as i64;
    while host.now() < deadline {
        let h = host.read_handoff();
        if pred(h.as_ref()) {
            return h;
        }
        if h.as_ref().map(|h| h.phase) == Some(HandoffPhase::Failed) {
            return h;
        }
        host.sleep_ms(100).await;
    }
    host.read_handoff()
}

pub async fn apply_update(
    cfg: &ApplierCfg,
    host: &dyn ApplierHost,
) -> anyhow::Result<ApplyOutcome> {
    host.log(&format!("update: applying {}", cfg.version));
    let staged = host.stage().await;
    let Some(canary_path) = staged.canary_path.filter(|_| staged.ok) else {
        host.log(&format!(
            "update: stage failed: {}",
            staged.error.as_deref().unwrap_or("unknown")
        ));
        return Ok(ApplyOutcome::Aborted);
    };

    // SPAWN MUST PRECEDE SWAP — the canary runs from `.ryuzi.canary` and (on
    // unix) holds its executable by inode, so the later rename can't disturb it.
    let pid = match host.spawn_canary(&canary_path) {
        Ok(pid) => pid,
        Err(e) => {
            host.log(&format!("update: canary spawn failed: {e}"));
            return Ok(ApplyOutcome::Aborted);
        }
    };

    let verdict = wait_for(cfg, host, |h| {
        h.map(|h| h.phase) == Some(HandoffPhase::Healthy)
    })
    .await;
    if verdict.as_ref().map(|h| h.phase) != Some(HandoffPhase::Healthy) {
        let why = verdict
            .and_then(|h| h.detail)
            .unwrap_or_else(|| "timeout".into());
        host.log(&format!(
            "update: canary unhealthy ({why}), staying on {}",
            cfg.version
        ));
        host.kill_canary(pid);
        host.clear_handoff();
        return Ok(ApplyOutcome::Aborted); // old daemon never stopped → zero downtime
    }

    // Green: finish in-flight turns, swap the binary, hand over.
    host.drain(cfg.drain_timeout_ms).await;
    host.backup()?;
    host.swap()?; // atomic rename .ryuzi.canary → ryuzi
    host.write_handoff(&Handoff {
        phase: HandoffPhase::Promote,
        pid,
        version: cfg.version.clone(),
        detail: None,
    });
    host.stop_gateways().await;

    // Watchdog: confirm the canary becomes the live daemon.
    let promoted = wait_for(cfg, host, |h| {
        h.map(|h| h.phase) == Some(HandoffPhase::Promoted)
    })
    .await;
    if promoted.map(|h| h.phase) == Some(HandoffPhase::Promoted) {
        host.log(&format!("update: promoted to {}", cfg.version));
        host.clear_handoff();
        return Ok(ApplyOutcome::Promoted);
    }

    host.log("update: canary failed to promote, rolling back to previous binary");
    host.kill_canary(pid);
    host.restore()?; // rename ryuzi.bak → ryuzi
    host.clear_handoff();
    Ok(ApplyOutcome::RolledBack)
}

pub fn handle_apply_outcome(
    outcome: ApplyOutcome,
    spawn_fresh_daemon: impl FnOnce(),
    exit: impl FnOnce(i32),
    log: impl Fn(&str),
) {
    match outcome {
        ApplyOutcome::Promoted => {
            log("update: handed over to the new daemon; exiting");
            exit(0);
        }
        ApplyOutcome::RolledBack => {
            log("update: rolled back; respawning the previous daemon");
            spawn_fresh_daemon();
            exit(0);
        }
        ApplyOutcome::Aborted => {} // old daemon keeps serving
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    struct ScriptedApplier {
        calls: Mutex<Vec<String>>,
        script: Mutex<(Vec<Option<Handoff>>, usize)>,
        stage_result: StageResult,
        now_step: i64,
        now: Mutex<i64>,
    }

    impl ScriptedApplier {
        fn new(script: Vec<Option<Handoff>>, stage_result: StageResult) -> Self {
            Self {
                calls: Mutex::new(Vec::new()),
                script: Mutex::new((script, 0)),
                stage_result,
                now_step: 0,
                now: Mutex::new(0),
            }
        }

        fn with_now_step(
            script: Vec<Option<Handoff>>,
            stage_result: StageResult,
            now_step: i64,
        ) -> Self {
            Self {
                calls: Mutex::new(Vec::new()),
                script: Mutex::new((script, 0)),
                stage_result,
                now_step,
                now: Mutex::new(0),
            }
        }
    }

    #[async_trait]
    impl ApplierHost for ScriptedApplier {
        async fn stage(&self) -> StageResult {
            self.calls.lock().unwrap().push("stage".into());
            self.stage_result.clone()
        }

        fn spawn_canary(&self, _canary_path: &Path) -> anyhow::Result<i32> {
            self.calls.lock().unwrap().push("spawnCanary".into());
            Ok(999)
        }

        fn read_handoff(&self) -> Option<Handoff> {
            let mut script = self.script.lock().unwrap();
            let (entries, idx) = &mut *script;
            let val = entries[std::cmp::min(*idx, entries.len() - 1)].clone();
            *idx += 1;
            val
        }

        fn write_handoff(&self, h: &Handoff) {
            self.calls.lock().unwrap().push(
                format!("handoff:{:?}", h.phase)
                    .replace("HandoffPhase::", "")
                    .to_lowercase(),
            );
        }

        fn clear_handoff(&self) {
            self.calls.lock().unwrap().push("clearHandoff".into());
        }

        async fn drain(&self, _timeout_ms: u64) {
            self.calls.lock().unwrap().push("drain".into());
        }

        fn backup(&self) -> anyhow::Result<()> {
            self.calls.lock().unwrap().push("backup".into());
            Ok(())
        }

        fn swap(&self) -> anyhow::Result<()> {
            self.calls.lock().unwrap().push("swap".into());
            Ok(())
        }

        fn restore(&self) -> anyhow::Result<()> {
            self.calls.lock().unwrap().push("restore".into());
            Ok(())
        }

        fn kill_canary(&self, _pid: i32) {
            self.calls.lock().unwrap().push("killCanary".into());
        }

        async fn stop_gateways(&self) {
            self.calls.lock().unwrap().push("stopGateways".into());
        }

        fn now(&self) -> i64 {
            let mut t = self.now.lock().unwrap();
            let prev = *t;
            *t += self.now_step;
            prev
        }

        async fn sleep_ms(&self, _ms: u64) {}

        fn log(&self, _m: &str) {}
    }

    #[tokio::test]
    async fn happy_path_exact_call_order() {
        let script = vec![
            Some(Handoff {
                phase: HandoffPhase::Probing,
                pid: 999,
                version: "0.3.0".into(),
                detail: None,
            }),
            Some(Handoff {
                phase: HandoffPhase::Healthy,
                pid: 999,
                version: "0.3.0".into(),
                detail: None,
            }),
            Some(Handoff {
                phase: HandoffPhase::Promoted,
                pid: 999,
                version: "0.3.0".into(),
                detail: None,
            }),
        ];
        let applier = ScriptedApplier::new(
            script,
            StageResult {
                ok: true,
                canary_path: Some("/home/me/.local/bin/.ryuzi.canary".into()),
                error: None,
            },
        );
        let cfg = ApplierCfg {
            version: "0.3.0".into(),
            drain_timeout_ms: 1000,
            canary_timeout_ms: 1000,
        };
        let outcome = apply_update(&cfg, &applier).await.unwrap();
        assert_eq!(outcome, ApplyOutcome::Promoted);
        assert_eq!(
            *applier.calls.lock().unwrap(),
            vec![
                "stage",
                "spawnCanary",
                "drain",
                "backup",
                "swap",
                "handoff:promote",
                "stopGateways",
                "clearHandoff"
            ]
        );
    }

    #[tokio::test]
    async fn canary_unhealthy_aborts_no_swap_old_daemon_keeps_serving() {
        let script = vec![
            Some(Handoff {
                phase: HandoffPhase::Probing,
                pid: 999,
                version: "0.3.0".into(),
                detail: None,
            }),
            Some(Handoff {
                phase: HandoffPhase::Failed,
                pid: 999,
                version: "0.3.0".into(),
                detail: Some("db".into()),
            }),
        ];
        let applier = ScriptedApplier::new(
            script,
            StageResult {
                ok: true,
                canary_path: Some("/home/me/.local/bin/.ryuzi.canary".into()),
                error: None,
            },
        );
        let cfg = ApplierCfg {
            version: "0.3.0".into(),
            drain_timeout_ms: 1000,
            canary_timeout_ms: 1000,
        };
        let outcome = apply_update(&cfg, &applier).await.unwrap();
        assert_eq!(outcome, ApplyOutcome::Aborted);
        let calls = applier.calls.lock().unwrap();
        assert!(calls.contains(&"killCanary".into()));
        assert!(calls.contains(&"clearHandoff".into()));
        assert!(!calls.contains(&"swap".to_string()));
        assert!(!calls.contains(&"stopGateways".to_string()));
    }

    #[tokio::test]
    async fn stage_failure_aborts_before_spawning() {
        let applier = ScriptedApplier::new(
            vec![],
            StageResult {
                ok: false,
                canary_path: None,
                error: Some("checksum".into()),
            },
        );
        let cfg = ApplierCfg {
            version: "0.3.0".into(),
            drain_timeout_ms: 1000,
            canary_timeout_ms: 1000,
        };
        let outcome = apply_update(&cfg, &applier).await.unwrap();
        assert_eq!(outcome, ApplyOutcome::Aborted);
        assert_eq!(*applier.calls.lock().unwrap(), vec!["stage"]);
    }

    #[tokio::test]
    async fn promote_never_confirmed_rolls_back() {
        let script = vec![
            Some(Handoff {
                phase: HandoffPhase::Probing,
                pid: 999,
                version: "0.3.0".into(),
                detail: None,
            }),
            Some(Handoff {
                phase: HandoffPhase::Healthy,
                pid: 999,
                version: "0.3.0".into(),
                detail: None,
            }),
            Some(Handoff {
                phase: HandoffPhase::Healthy,
                pid: 999,
                version: "0.3.0".into(),
                detail: None,
            }),
        ];
        let applier = ScriptedApplier::with_now_step(
            script,
            StageResult {
                ok: true,
                canary_path: Some("/home/me/.local/bin/.ryuzi.canary".into()),
                error: None,
            },
            600, // advance 600ms per now() call, so we timeout past canary_timeout_ms: 1000
        );
        let cfg = ApplierCfg {
            version: "0.3.0".into(),
            drain_timeout_ms: 1000,
            canary_timeout_ms: 1000,
        };
        let outcome = apply_update(&cfg, &applier).await.unwrap();
        assert_eq!(outcome, ApplyOutcome::RolledBack);
        let calls = applier.calls.lock().unwrap();
        assert!(calls.contains(&"restore".into()));
        assert!(calls.contains(&"clearHandoff".into()));
    }

    #[test]
    fn handle_apply_outcome_matrix() {
        use std::cell::RefCell;

        // Test Promoted → exit(0) no spawn
        let record = RefCell::new(Vec::new());
        handle_apply_outcome(
            ApplyOutcome::Promoted,
            || record.borrow_mut().push("spawn"),
            |code| {
                assert_eq!(code, 0);
                record.borrow_mut().push("exit");
            },
            |_| {},
        );
        assert_eq!(*record.borrow(), vec!["exit"]);

        // Test RolledBack → spawn then exit(0)
        let record = RefCell::new(Vec::new());
        handle_apply_outcome(
            ApplyOutcome::RolledBack,
            || record.borrow_mut().push("spawn"),
            |code| {
                assert_eq!(code, 0);
                record.borrow_mut().push("exit");
            },
            |_| {},
        );
        assert_eq!(*record.borrow(), vec!["spawn", "exit"]);

        // Test Aborted → neither spawn nor exit
        let record = RefCell::new(Vec::new());
        handle_apply_outcome(
            ApplyOutcome::Aborted,
            || record.borrow_mut().push("spawn"),
            |_code| {
                record.borrow_mut().push("exit");
            },
            |_| {},
        );
        assert!(record.borrow().is_empty());
    }
}
