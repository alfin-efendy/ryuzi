//! Canary probe and promote handshake — port of `apps/cli/src/cli/update-canary.ts`.
//! Coordinates a staged daemon rollout: probe the DB on the new binary, wait for an
//! applier signal to promote, then return success or failure for watchdog rollback.

use crate::update::handoff::{Handoff, HandoffPhase};

#[derive(Debug, Clone, PartialEq)]
pub struct ProbeResult {
    pub healthy: bool,
    pub detail: Option<String>,
}

/// Version check FIRST (TS parity) — callers short-circuit open_db when the
/// versions already mismatch (see run_canary_with).
pub fn probe(open_db: anyhow::Result<()>, version: &str, target_version: &str) -> ProbeResult {
    if version != target_version {
        return ProbeResult {
            healthy: false,
            detail: Some(format!(
                "version mismatch: running {}, expected {}",
                version, target_version
            )),
        };
    }
    if let Err(e) = open_db {
        return ProbeResult {
            healthy: false,
            detail: Some(format!("db open failed: {}", e)),
        };
    }
    ProbeResult {
        healthy: true,
        detail: None,
    }
}

pub fn canary_target_version(current_version: &str, env: Option<String>) -> String {
    env.unwrap_or_else(|| current_version.to_string())
}

pub fn canary_timeout_ms(env: Option<String>) -> u64 {
    env.and_then(|v| v.parse().ok()).unwrap_or(60_000)
}

pub struct CanaryCfg {
    pub version: String,
    pub target_version: String,
    pub pid: i32,
    pub timeout_ms: u64,
}

#[async_trait::async_trait]
pub trait CanaryHost: Send + Sync {
    async fn open_db(&self) -> anyhow::Result<()>;
    async fn promote(&self) -> anyhow::Result<()>;
    fn write_handoff(&self, h: &Handoff);
    fn read_handoff(&self) -> Option<Handoff>;
    fn now(&self) -> i64;
    async fn sleep_ms(&self, ms: u64);
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum CanaryOutcome {
    Promoted,
    Failed,
}

pub async fn run_canary_with(cfg: &CanaryCfg, host: &dyn CanaryHost) -> CanaryOutcome {
    let hand = |phase, detail: Option<String>| Handoff {
        phase,
        pid: cfg.pid,
        version: cfg.version.clone(),
        detail,
    };
    host.write_handoff(&hand(HandoffPhase::Probing, None));

    // Version check first so a mismatched canary never opens the db (TS probe order).
    let open = if cfg.version == cfg.target_version {
        host.open_db().await
    } else {
        Ok(())
    };
    let p = probe(open, &cfg.version, &cfg.target_version);
    if !p.healthy {
        host.write_handoff(&hand(HandoffPhase::Failed, p.detail));
        return CanaryOutcome::Failed;
    }
    host.write_handoff(&hand(HandoffPhase::Healthy, None));

    let deadline = host.now() + cfg.timeout_ms as i64;
    while host.now() < deadline {
        if host.read_handoff().map(|h| h.phase) == Some(HandoffPhase::Promote) {
            // Delta from TS (which crashed on a promote() rejection): record
            // the failure so the applier's watchdog rolls back deterministically.
            if let Err(e) = host.promote().await {
                host.write_handoff(&hand(
                    HandoffPhase::Failed,
                    Some(format!("promote failed: {}", e)),
                ));
                return CanaryOutcome::Failed;
            }
            host.write_handoff(&hand(HandoffPhase::Promoted, None));
            return CanaryOutcome::Promoted;
        }
        host.sleep_ms(100).await;
    }
    host.write_handoff(&hand(HandoffPhase::Failed, Some("promote timeout".into())));
    CanaryOutcome::Failed
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Mutex;

    struct ScriptedHost {
        script: Vec<Option<Handoff>>,
        cursor: Mutex<usize>,
        written: Mutex<Vec<Handoff>>,
        promoted: AtomicUsize,
        open_db_called: AtomicUsize,
        now_val: Mutex<i64>,
        now_step: u64,
    }

    impl ScriptedHost {
        fn new(script: Vec<Option<Handoff>>) -> Self {
            Self {
                script,
                cursor: Mutex::new(0),
                written: Mutex::new(Vec::new()),
                promoted: AtomicUsize::new(0),
                open_db_called: AtomicUsize::new(0),
                now_val: Mutex::new(0),
                now_step: 0,
            }
        }

        fn with_now_step(script: Vec<Option<Handoff>>, step: u64) -> Self {
            Self {
                script,
                cursor: Mutex::new(0),
                written: Mutex::new(Vec::new()),
                promoted: AtomicUsize::new(0),
                open_db_called: AtomicUsize::new(0),
                now_val: Mutex::new(0),
                now_step: step,
            }
        }
    }

    #[async_trait::async_trait]
    impl CanaryHost for ScriptedHost {
        async fn open_db(&self) -> anyhow::Result<()> {
            self.open_db_called.fetch_add(1, Ordering::SeqCst);
            Ok(())
        }

        async fn promote(&self) -> anyhow::Result<()> {
            self.promoted.fetch_add(1, Ordering::SeqCst);
            Ok(())
        }

        fn write_handoff(&self, h: &Handoff) {
            self.written.lock().unwrap().push(h.clone());
        }

        fn read_handoff(&self) -> Option<Handoff> {
            let mut cur = self.cursor.lock().unwrap();
            let idx = std::cmp::min(*cur, self.script.len().saturating_sub(1));
            *cur += 1;
            self.script.get(idx).and_then(|h| h.clone())
        }

        fn now(&self) -> i64 {
            if self.now_step > 0 {
                let mut t = self.now_val.lock().unwrap();
                *t += self.now_step as i64;
                *t
            } else {
                0
            }
        }

        async fn sleep_ms(&self, _ms: u64) {
            // no-op
        }
    }

    #[test]
    fn probe_is_healthy_when_version_matches_and_db_opens() {
        let result = probe(Ok(()), "0.3.0", "0.3.0");
        assert_eq!(
            result,
            ProbeResult {
                healthy: true,
                detail: None,
            }
        );
    }

    #[test]
    fn probe_fails_on_version_mismatch() {
        let result = probe(Ok(()), "0.2.0", "0.3.0");
        assert!(!result.healthy);
        assert!(result.detail.is_some());
        let detail = result.detail.unwrap();
        assert!(detail.contains("version mismatch"));
        assert!(detail.contains("0.2.0"));
        assert!(detail.contains("0.3.0"));
    }

    #[test]
    fn probe_fails_when_db_cannot_open() {
        let err = anyhow::anyhow!("locked");
        let result = probe(Err(err), "0.3.0", "0.3.0");
        assert!(!result.healthy);
        assert!(result.detail.is_some());
        let detail = result.detail.unwrap();
        assert!(detail.contains("db open failed"));
        assert!(detail.contains("locked"));
    }

    #[test]
    fn canary_target_version_reads_the_env_override() {
        assert_eq!(
            canary_target_version("0.2.0", Some("0.3.0".into())),
            "0.3.0"
        );
        assert_eq!(canary_target_version("0.2.0", None), "0.2.0");
    }

    #[test]
    fn canary_timeout_reads_env_and_defaults_to_60s() {
        assert_eq!(canary_timeout_ms(Some("1234".into())), 1234);
        assert_eq!(canary_timeout_ms(None), 60_000);
        assert_eq!(canary_timeout_ms(Some("junk".into())), 60_000);
    }

    #[tokio::test]
    async fn healthy_then_promote_promotes() {
        let host = ScriptedHost::new(vec![
            Some(Handoff {
                phase: HandoffPhase::Healthy,
                pid: 1,
                version: "0.3.0".into(),
                detail: None,
            }),
            Some(Handoff {
                phase: HandoffPhase::Promote,
                pid: 1,
                version: "0.3.0".into(),
                detail: None,
            }),
        ]);

        let cfg = CanaryCfg {
            version: "0.3.0".into(),
            target_version: "0.3.0".into(),
            pid: 1,
            timeout_ms: 1000,
        };

        let outcome = run_canary_with(&cfg, &host).await;

        assert_eq!(outcome, CanaryOutcome::Promoted);
        assert_eq!(host.promoted.load(Ordering::SeqCst), 1);

        let written = host.written.lock().unwrap();
        let phases: Vec<_> = written.iter().map(|h| h.phase).collect();
        assert_eq!(
            phases,
            vec![
                HandoffPhase::Probing,
                HandoffPhase::Healthy,
                HandoffPhase::Promoted
            ]
        );
    }

    #[tokio::test]
    async fn probe_failure_writes_failed_never_promotes() {
        let host = ScriptedHost::new(vec![]);

        let cfg = CanaryCfg {
            version: "0.2.0".into(),
            target_version: "0.3.0".into(),
            pid: 1,
            timeout_ms: 1000,
        };

        let outcome = run_canary_with(&cfg, &host).await;

        assert_eq!(outcome, CanaryOutcome::Failed);
        assert_eq!(host.promoted.load(Ordering::SeqCst), 0);
        assert_eq!(host.open_db_called.load(Ordering::SeqCst), 0); // Version short-circuit

        let written = host.written.lock().unwrap();
        let phases: Vec<_> = written.iter().map(|h| h.phase).collect();
        assert_eq!(phases, vec![HandoffPhase::Probing, HandoffPhase::Failed]);
    }

    #[tokio::test]
    async fn promote_never_arrives_times_out_failed() {
        let host = ScriptedHost::with_now_step(
            vec![Some(Handoff {
                phase: HandoffPhase::Healthy,
                pid: 1,
                version: "0.3.0".into(),
                detail: None,
            })],
            600,
        );

        let cfg = CanaryCfg {
            version: "0.3.0".into(),
            target_version: "0.3.0".into(),
            pid: 1,
            timeout_ms: 1000,
        };

        let outcome = run_canary_with(&cfg, &host).await;

        assert_eq!(outcome, CanaryOutcome::Failed);
        assert_eq!(host.promoted.load(Ordering::SeqCst), 0);

        let written = host.written.lock().unwrap();
        let phases: Vec<_> = written.iter().map(|h| h.phase).collect();
        assert_eq!(
            phases,
            vec![
                HandoffPhase::Probing,
                HandoffPhase::Healthy,
                HandoffPhase::Failed
            ]
        );

        let last_detail = written.last().unwrap().detail.as_deref();
        assert_eq!(last_detail, Some("promote timeout"));
    }
}
