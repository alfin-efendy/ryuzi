//! Settings schema, provider catalog, and validated settings store facade.

pub mod catalog;
pub mod fields;
pub mod store;

pub use catalog::{all_fields, find_field, is_secret, CATALOG};
pub use catalog::{GatewayDescriptor, ProviderCatalog, RuntimeDescriptor};
pub use fields::{ConfigField, FieldType, GLOBAL_FIELDS};
pub use store::{csv, validate_setting, SettingsStore};

/// Read a numeric setting from the KV store with a default and a floor of 1.
/// The one shared reader for capacity-style settings (`max_concurrent_runs`,
/// `max_spawn_depth`) so their parse/default/floor behavior cannot drift.
pub async fn usize_setting(store: &crate::store::Store, key: &str, default: usize) -> usize {
    store
        .get_setting(key)
        .await
        .ok()
        .flatten()
        .and_then(|v| v.parse::<usize>().ok())
        .unwrap_or(default)
        .max(1)
}

/// Expand a leading `~` / `~/` to `$HOME`. `~` is a shell-ism; Rust path ops
/// and git do NOT expand it, so a stored path like `workdir_root: ~/repos`
/// must be expanded before use or it becomes a literal `~` directory and
/// breaks worktree/cwd resolution. Lives here (rather than in the CLI) so
/// `ControlPlane` can share it too.
pub fn expand_home(dir: &str) -> std::path::PathBuf {
    if let Some(rest) = dir.strip_prefix('~') {
        if let Some(home) = std::env::var_os("HOME") {
            return std::path::PathBuf::from(home).join(rest.trim_start_matches('/'));
        }
    }
    std::path::PathBuf::from(dir)
}

#[cfg(test)]
mod expand_home_tests {
    use super::expand_home;
    use serial_test::serial;

    /// `HOME` is process-global — `#[serial]` so this doesn't race other
    /// tests that read/set it (e.g. `control::tests::StateDirGuard`).
    #[test]
    #[serial]
    fn expands_leading_tilde_variants() {
        std::env::set_var("HOME", "/home/u");
        assert_eq!(expand_home("~"), std::path::PathBuf::from("/home/u"));
        assert_eq!(
            expand_home("~/repos"),
            std::path::PathBuf::from("/home/u/repos")
        );
        assert_eq!(
            expand_home("/already/abs"),
            std::path::PathBuf::from("/already/abs")
        );
    }
}
