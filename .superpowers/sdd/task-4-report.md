# Task 4 Report

Status: DONE

Summary:
- Extended `AuthSpec` in `crates/plugin-sdk/src/manifest.rs` with OAuth metadata fields, defaults, and `#[serde(default, rename_all = "kebab-case")]`.
- Added serde `alias = "help_url"` on `help_url` to preserve existing manifest compatibility.
- Added OAuth-focused tests for full metadata parsing and `[auth] kind = "oauth"` compatibility with missing OAuth-specific fields.

Verification:
- `cargo test -p ryuzi-plugin-sdk`

Follow-up fixes:
- Updated all downstream `AuthSpec { ... }` struct literals in Rust to compile with new fields by using `..Default::default()`, including:
  - `crates/core/src/plugins/providers.rs`
  - `crates/core/src/settings/catalog.rs`
  - `crates/core/src/settings/store.rs`
  - `crates/core/src/control/tests.rs`
- Added a manifest unit test `parses_canonical_help_url_key` in
  `crates/plugin-sdk/src/manifest.rs` to assert canonical `help-url` is parsed.

Verification (re-run):
- `cargo test -p ryuzi-plugin-sdk`
- `cargo check -p ryuzi-core`
