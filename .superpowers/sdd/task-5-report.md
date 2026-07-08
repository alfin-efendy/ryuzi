Status: DONE

Summary:
- Added `crates/core/src/plugins/oauth.rs` with `PluginOauthToken`, PKCE verifier/challenge helpers, five-minute `needs_refresh`, and `WWW-Authenticate` resource parsing that prefers `resource_metadata`.
- Exported the new `plugins::oauth` module from `crates/core/src/plugins/mod.rs`.
- Added the `plugin_oauth_tokens` store migration plus `Store` helpers to upsert, read, reconnect-mark, and delete plugin OAuth tokens while encrypting `access_token` and `refresh_token` inside `token_json`.
- Preserved unknown JSON fields on upsert/reconnect updates and added focused store tests that verify ciphertext-at-rest, roundtrip decryption, reconnect marking, unknown-field preservation, and delete behavior.

Verification:
- `cargo test -p ryuzi-core plugins::oauth`
- `cargo test -p ryuzi-core plugin_oauth`

Notes:
- `needs_refresh(now, None)` is treated as due immediately and covered by unit tests.
- Store tests use `llm_router::secrets::use_test_key_file()` so the process-global cipher stays hermetic, and they verify encryption by asserting raw `token_json` does not contain plaintext secrets.
- `parse_www_authenticate_resource` now skips a leading authentication scheme token (for example, `Bearer`) before parsing auth params so headers like `Bearer resource="..."` and `Bearer resource_metadata="..."` are parsed correctly.
- Added regression tests for:
  - `Bearer resource="https://api.example.test"`
  - `Bearer resource_metadata="https://api.example.test/.well-known/oauth-protected-resource"`

Follow-up fixes:
- Fixed `parse_www_authenticate_resource` for multi-challenge headers by treating alphabetic tokens without `=` as per-challenge auth schemes, so `Basic realm="x", Bearer resource="https://api.example.test"` now resolves the Bearer resource correctly without regressing quoted commas, escapes, unquoted values, or `resource_metadata` precedence.
- Fixed `Store::mark_plugin_oauth_reconnect_required` to normalize legacy plaintext `access_token` and `refresh_token` values through the same decode + `upsert_plugin_oauth_token_json` path used by upsert, preserving unknown JSON fields while rewriting the row with encrypted token values and `reconnect_required = true`.
- Added regression coverage for both paths:
  - `plugins::oauth::tests::parse_www_authenticate_reads_bearer_resource_after_another_challenge`
  - `store::tests::mark_plugin_oauth_reconnect_required_normalizes_legacy_plaintext_tokens`

Follow-up verification:
- `cargo fmt`
- `cargo test -p ryuzi-core plugins::oauth`
- `cargo test -p ryuzi-core plugin_oauth`
