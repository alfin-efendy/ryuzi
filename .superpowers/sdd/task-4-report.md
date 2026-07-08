# Task 4 Report

Status: DONE

Summary:
- Extended `AuthSpec` in `crates/plugin-sdk/src/manifest.rs` with OAuth metadata fields, defaults, and `#[serde(default, rename_all = "kebab-case")]`.
- Added serde `alias = "help_url"` on `help_url` to preserve existing manifest compatibility.
- Added OAuth-focused tests for full metadata parsing and `[auth] kind = "oauth"` compatibility with missing OAuth-specific fields.

Verification:
- `cargo test -p ryuzi-plugin-sdk`
