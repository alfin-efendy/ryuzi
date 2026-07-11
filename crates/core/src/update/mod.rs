//! Self-update machinery: version/check/asset/install-method (deciding
//! whether and what to download) and manager/stage/canary/applier/handoff
//! (driving the staged, canary-verified binary swap).
//! Logic lives here behind injectable traits; `crates/runner/src/daemon_cmd.rs`
//! provides the production impls (real HTTP, real tar, real spawn/renames).
pub mod applier;
pub mod asset;
pub mod canary;
pub mod check;
pub mod handoff;
pub mod install_method;
pub mod manager;
pub mod stage;
pub mod version;

pub use applier::{apply_update, handle_apply_outcome, ApplierCfg, ApplierHost, ApplyOutcome};
pub use asset::{
    asset_name, asset_url, checksums_url, detect_platform, platform_tag, sha256_hex,
    verify_checksum, Platform,
};
pub use canary::{
    canary_target_version, canary_timeout_ms, probe, run_canary_with, CanaryCfg, CanaryHost,
    CanaryOutcome, ProbeResult,
};
pub use check::{check_for_update, HttpResponse, UpdateCheckResult, UpdateHttp, UreqHttp};
pub use handoff::{
    clear_handoff, handoff_path, read_handoff, write_handoff, Handoff, HandoffPhase,
};
pub use install_method::{detect_install_method, InstallInfo, InstallMethod};
pub use manager::{
    upgrade_hint, ApplyHook, ApplyInfo, NotifyTarget, UpdateManager, UpdateManagerDeps, UpdateMode,
    DEFAULT_CHECK_INTERVAL_MS, DEFAULT_REPO,
};
pub use stage::{stage_canary, StageHost, StageOpts, StageResult, TarStageHost};
pub use version::{compare_versions, is_newer, parse_version, SemVer};
