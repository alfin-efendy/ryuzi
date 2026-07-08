Status: DONE

Summary:
- Added focused `crates/core/src/plugins/declarative.rs` coverage for HTTP OAuth bearer injection from stored plugin tokens, missing/reconnect-required auth gating for HTTP OAuth manifests, and the existing stdio `${auth}` path for OAuth manifests like Google Workspace.
- Extended `PreloadedResolver` with `oauth_bearer_token`, loaded stored plugin OAuth tokens through a minimal `SettingsStore::store()` accessor, and injected/replaced `Authorization: Bearer <token>` only for `McpTransportDef::Http` after header template resolution.
- Kept API-key/token `${auth}` substitution unchanged and returned clear `configure <plugin-id>: ...` errors for HTTP OAuth plugins when the stored token is missing or marked `reconnect_required`.
- Added discovery-oriented OAuth metadata (`resource` plus `dynamic-registration = true`) to the HTTP OAuth catalog manifests for Atlassian, Cloudflare, Figma, Notion, and Sentry, while leaving `google-workspace.toml` unchanged.
- Addressed review finding #6 by removing `dynamic-registration = true` from `crates/core/plugins/catalog/vercel.toml` and `crates/core/plugins/catalog/slack.toml` because their descriptions indicate OAuth flows are not broadly self-service.

Verification:
- `cargo fmt`
- `cargo test -p ryuzi-core declarative`
- `cargo check -p ryuzi-core`
