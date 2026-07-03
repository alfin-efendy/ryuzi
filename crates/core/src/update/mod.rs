//! Self-update machinery (Spec 4 slice 5) — port of the retired TS updater:
//! `packages/core/src/update/*` (version/check/asset/install-method) and
//! `apps/cli/src/cli/update-*.ts` (manager/stage/canary/applier/handoff).
//! Logic lives here behind injectable traits; `crates/cli/src/daemon_cmd.rs`
//! provides the production impls (real HTTP, real tar, real spawn/renames).
pub mod version;

pub use version::{compare_versions, is_newer, parse_version, SemVer};
