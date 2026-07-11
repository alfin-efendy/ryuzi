//! Tool provision (Track D, DT6): an extension that declared `provides_tools`
//! returns tool defs at `extension/initialize` (opaque `Value`s, captured by
//! `ExtensionProc::tools`/`ExtensionSnapshot::tools`); this module gives them
//! a type ([`ExtensionToolDef`]) and gathers every currently-provided tool,
//! across every plugin, into an [`ExtensionToolBinding`] — everything
//! `harness::native::tools::extension::ExtensionTool` needs to wrap one as a
//! native `Tool` (naming, description/schema, the owning plugin's
//! [`Principal`] for approval attribution, and a caller to actually dispatch
//! `tool/call` — see `proc::ExtensionCaller`).
//!
//! [`ExtensionTools::session_tools`] is [`super::events::ExtensionEvents::dispatch`]'s
//! sibling accessor: `SessionCtx` threads a SECOND
//! `Option<Arc<dyn ExtensionTools>>` (`extension_tools`) alongside
//! `extension_events`, both resolved from the same daemon-global
//! [`super::ExtensionHost`] at session start
//! (`ControlPlane::start_harness_session`) — `None` in the common case (no
//! extensions spawned) and in every bare test `SessionCtx`, so a session with
//! no extensions pays zero extra cost building its tool registry, exactly
//! like every hook fire site already pays zero extra cost dispatching events.
//!
//! An extension that is not `Running`, or is running but never declared
//! `provides_tools`, or whose declared tool list is empty, contributes
//! NOTHING — see [`ExtensionTools::session_tools`]'s filtering. A malformed
//! tool def (missing/blank `name`) is skipped with a warning, never a crash —
//! an extension is untrusted, arbitrary vendor code (see `plugins::extension`'s
//! module doc), so its declared tool list gets the same "must not crash the
//! host" treatment DT3-DT5 already give every other extension response.

use async_trait::async_trait;
use serde_json::Value;

use crate::domain::Principal;

use super::proc::{ExtensionCaller, ExtensionHost};
use super::ExtensionStatus;

/// A typed extension-declared tool definition, parsed from
/// `extension/initialize`'s raw `tools` array by [`parse_tool_def`].
#[derive(Debug, Clone, PartialEq)]
pub struct ExtensionToolDef {
    pub name: String,
    pub description: String,
    pub input_schema: Value,
}

/// Parse one raw tool def. Only `name` is required (non-empty after
/// trimming) — the one field a native `Tool` cannot function without, since
/// both wire naming (`ext__<extension>__<name>`) and the `tool/call` dispatch
/// itself key off it. `description` defaults to `""`; `input_schema` defaults
/// to a bare permissive `{"type":"object"}` when absent or not a JSON object
/// — an extension is untrusted input, but a thin/missing schema is merely
/// imprecise, not unsafe, so it does not disqualify the whole def the way a
/// missing name does. Returns `None` (never panics) for anything that isn't
/// even a JSON object, or whose `name` is missing, blank, or not a string —
/// `serde_json::Value::get` returns `None` for a non-object/array `raw`
/// (e.g. a bare string or number), so this falls through safely rather than
/// panicking on a malformed entry.
pub(crate) fn parse_tool_def(raw: &Value) -> Option<ExtensionToolDef> {
    let name = raw.get("name").and_then(Value::as_str)?.trim();
    if name.is_empty() {
        return None;
    }
    let description = raw
        .get("description")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string();
    let input_schema = raw
        .get("inputSchema")
        .or_else(|| raw.get("input_schema"))
        .filter(|v| v.is_object())
        .cloned()
        .unwrap_or_else(|| serde_json::json!({ "type": "object" }));
    Some(ExtensionToolDef {
        name: name.to_string(),
        description,
        input_schema,
    })
}

/// The wire tool name `harness::native::tools::extension::ExtensionTool`
/// exposes to the model — `ext__<extension>__<tool>`. Single source of truth
/// shared by [`ExtensionTools::session_tools`]'s collision dedup (below) and
/// `ExtensionTool::from_binding`'s own naming, so the two can never drift.
pub(crate) fn full_tool_name(extension_name: &str, tool_name: &str) -> String {
    format!("ext__{extension_name}__{tool_name}")
}

/// Everything `harness::native::tools::extension::ExtensionTool` needs to
/// wrap one extension-provided tool: the typed def, the owning extension's
/// name (for `ext__<extension>__<tool>` naming — kept separate from
/// `principal.plugin_id`/`.plugin_name`, since one plugin may declare more
/// than one `[[extension]]`), the owning plugin's [`Principal`] (resolved
/// once at spawn time from the `CorePlugin` binding — never string-parsed
/// from a name), and a caller to actually dispatch `tool/call`.
pub struct ExtensionToolBinding {
    pub def: ExtensionToolDef,
    pub extension_name: String,
    pub principal: Principal,
    pub(crate) caller: std::sync::Arc<dyn ExtensionCaller>,
}

/// Gather every currently-provided extension tool, across every plugin — the
/// `ExtensionEvents`-sibling accessor `harness::native`'s session-start tool
/// gathering (`connect_extension_tools`, mirroring `connect_mcp_tools`) calls
/// through `SessionCtx.extension_tools`. Implemented by [`ExtensionHost`];
/// `None`/no host, and a host with nothing spawned, are both true no-ops —
/// see this module's doc.
#[async_trait]
pub trait ExtensionTools: Send + Sync {
    async fn session_tools(&self) -> Vec<ExtensionToolBinding>;
}

#[async_trait]
impl ExtensionTools for ExtensionHost {
    async fn session_tools(&self) -> Vec<ExtensionToolBinding> {
        // `ExtensionSpec::name` is only unique WITHIN one plugin's own
        // manifest, not globally (see its own doc) — two different plugins
        // can each declare an `[[extension]]`/tool pair that formats to the
        // identical `ext__<extension>__<tool>` full name. Left unguarded,
        // `harness::native::tools::ToolRegistry::with_extra`'s plain
        // `BTreeMap::insert` would let the later one silently shadow the
        // earlier with no log, and — because `tool_provision_entries` used
        // to walk raw `HashMap` iteration order — WHICH one "later" meant
        // was randomly reseeded every process start.
        // `tool_provision_entries` now returns entries pre-sorted by
        // `(plugin_id, extension name)`, so iterating it in order here and
        // tracking already-emitted full names is enough to make the winner
        // of a collision deterministic and stable across restarts: the
        // first entry (by that sort) to claim a full name always wins,
        // mirroring `ControlPlane::attach_plugin_mcp_servers`'s own
        // first-registration-wins `HashSet` discipline for MCP server names.
        let mut seen_full_names: std::collections::HashSet<String> =
            std::collections::HashSet::new();
        let mut out = Vec::new();
        for entry in self.tool_provision_entries().await {
            if !entry.provides_tools || !matches!(entry.status, ExtensionStatus::Running) {
                continue;
            }
            for raw in &entry.tools {
                match parse_tool_def(raw) {
                    Some(def) => {
                        let full_name = full_tool_name(&entry.name, &def.name);
                        if !seen_full_names.insert(full_name.clone()) {
                            tracing::warn!(
                                full_name = %full_name,
                                extension = %entry.name,
                                plugin = %entry.principal.plugin_id,
                                "skipping extension tool: full name already claimed by an earlier plugin's extension"
                            );
                            continue;
                        }
                        out.push(ExtensionToolBinding {
                            def,
                            extension_name: entry.name.clone(),
                            principal: entry.principal.clone(),
                            caller: entry.caller.clone(),
                        });
                    }
                    None => {
                        tracing::warn!(
                            extension = %entry.name,
                            tool_def = %raw,
                            "skipping malformed tool def from extension/initialize"
                        );
                    }
                }
            }
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::plugins::extension::{ExtensionCtx, ExtensionFactory, ExtensionSpec};
    use crate::plugins::host::{CorePlugin, PluginHost, PluginSource};
    use crate::settings::SettingsStore;
    use crate::store::Store;
    use ryuzi_plugin_sdk::PluginManifest;
    use serde_json::json;
    use std::time::Duration;

    // ---------- parse_tool_def (pure, no I/O) ----------

    #[test]
    fn parse_tool_def_accepts_a_full_valid_def() {
        let raw = json!({
            "name": "lint",
            "description": "Lint a file",
            "inputSchema": { "type": "object", "properties": {} }
        });
        let def = parse_tool_def(&raw).expect("a well-formed def must parse");
        assert_eq!(def.name, "lint");
        assert_eq!(def.description, "Lint a file");
        assert_eq!(
            def.input_schema,
            json!({ "type": "object", "properties": {} })
        );
    }

    #[test]
    fn parse_tool_def_defaults_missing_description_and_schema() {
        let raw = json!({ "name": "lint" });
        let def = parse_tool_def(&raw).unwrap();
        assert_eq!(def.description, "");
        assert_eq!(def.input_schema, json!({ "type": "object" }));
    }

    #[test]
    fn parse_tool_def_accepts_snake_case_input_schema_key() {
        let raw = json!({ "name": "lint", "input_schema": { "type": "object", "extra": true } });
        let def = parse_tool_def(&raw).unwrap();
        assert_eq!(def.input_schema, json!({ "type": "object", "extra": true }));
    }

    #[test]
    fn parse_tool_def_rejects_a_missing_name() {
        let raw = json!({ "description": "no name here" });
        assert!(parse_tool_def(&raw).is_none());
    }

    #[test]
    fn parse_tool_def_rejects_a_blank_name() {
        let raw = json!({ "name": "   " });
        assert!(parse_tool_def(&raw).is_none());
    }

    #[test]
    fn parse_tool_def_rejects_a_non_object_entry_without_panicking() {
        assert!(parse_tool_def(&json!("just a string")).is_none());
        assert!(parse_tool_def(&json!(42)).is_none());
        assert!(parse_tool_def(&json!(null)).is_none());
        assert!(parse_tool_def(&json!(["array", "entry"])).is_none());
    }

    #[test]
    fn parse_tool_def_ignores_a_non_object_input_schema() {
        let raw = json!({ "name": "lint", "inputSchema": "not an object" });
        let def = parse_tool_def(&raw).unwrap();
        assert_eq!(def.input_schema, json!({ "type": "object" }));
    }

    // ---------- ExtensionTools::session_tools (real sh-based fake extensions) ----------
    // Mirrors `events.rs`'s own integration test style: a tiny `sh -c`
    // one-liner plays the fake extension over real stdio pipes, hermetic (no
    // committed script file) and `#[cfg(unix)]`-gated to match this crate's
    // CI matrix. Unlike `events.rs`'s tests (which need a SECOND round trip
    // for the event dispatch), tool defs come back as part of the
    // `extension/initialize` response itself, so a single ack is enough to
    // exercise gathering.

    fn manifest(id: &str, name: &str) -> PluginManifest {
        PluginManifest {
            contract: 1,
            id: id.to_string(),
            name: name.to_string(),
            version: String::new(),
            publisher: String::new(),
            description: String::new(),
            homepage: None,
            icon: None,
            categories: vec![],
            slot: None,
            verified: false,
            experimental: false,
            auth: None,
            settings: vec![],
            mcp: vec![],
            extensions: vec![],
            skills: vec![],
            provider: None,
        }
    }

    struct FakeExtensionFactory {
        specs: Vec<ExtensionSpec>,
    }

    #[async_trait]
    impl ExtensionFactory for FakeExtensionFactory {
        async fn extensions(&self, _ctx: &ExtensionCtx) -> anyhow::Result<Vec<ExtensionSpec>> {
            Ok(self.specs.clone())
        }
    }

    fn extension_only(id: &str, name: &str, specs: Vec<ExtensionSpec>) -> CorePlugin {
        CorePlugin {
            manifest: manifest(id, name),
            harness: None,
            gateway: None,
            connector: None,
            extension: Some(std::sync::Arc::new(FakeExtensionFactory { specs })),
            source: PluginSource::Builtin,
        }
    }

    async fn open_ctx() -> (ExtensionCtx, std::sync::Arc<Store>, tempfile::NamedTempFile) {
        let tmp = tempfile::NamedTempFile::new().unwrap();
        let store = std::sync::Arc::new(Store::open(tmp.path()).await.unwrap());
        let settings = SettingsStore::new(store.clone());
        (ExtensionCtx { settings }, store, tmp)
    }

    fn base_spec(name: &str, body: &str, provides_tools: bool, timeout: Duration) -> ExtensionSpec {
        ExtensionSpec {
            name: name.to_string(),
            command: "sh".to_string(),
            args: vec!["-c".to_string(), body.to_string()],
            events: vec![],
            provides_tools,
            timeout,
            env: vec![],
        }
    }

    /// A `sh` script: read the `extension/initialize` request, ack it with
    /// `tools: <tools_json>` (a raw JSON array literal), then block waiting
    /// for further input (so the process stays alive for `shutdown_all`).
    fn init_ack_with_tools(tools_json: &str) -> String {
        format!(
            "IFS= read -r line; id=$(printf '%s' \"$line\" | sed -n 's/.*\"id\":\\([0-9]*\\).*/\\1/p'); \
             printf '{{\"jsonrpc\":\"2.0\",\"id\":%s,\"result\":{{\"ok\":true,\"events\":[],\"tools\":{tools}}}}}\\n' \"$id\"; \
             cat > /dev/null",
            tools = tools_json,
        )
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn session_tools_wraps_a_running_provides_tools_extensions_tool_defs() {
        let (ctx, store, _tmp) = open_ctx().await;
        let mut host = PluginHost::new();
        let body = init_ack_with_tools(
            r#"[{"name":"lint","description":"lint code","inputSchema":{"type":"object"}}]"#,
        );
        host.add(extension_only(
            "linter-plugin",
            "Linter Plugin",
            vec![base_spec("linter", &body, true, Duration::from_millis(500))],
        ));
        store
            .set_setting_raw("plugin.linter-plugin.enabled", "true")
            .await
            .unwrap();

        let ext_host = ExtensionHost::new();
        ext_host.spawn_all(&host, &ctx).await;
        assert_eq!(
            ext_host.get("linter-plugin").await[0].status,
            crate::plugins::extension::ExtensionStatus::Running,
            "sanity: the fake extension must have handshaken successfully"
        );

        let bindings = ext_host.session_tools().await;
        assert_eq!(bindings.len(), 1);
        assert_eq!(bindings[0].def.name, "lint");
        assert_eq!(bindings[0].def.description, "lint code");
        assert_eq!(bindings[0].extension_name, "linter");
        assert_eq!(bindings[0].principal.plugin_id, "linter-plugin");
        assert_eq!(bindings[0].principal.plugin_name, "Linter Plugin");

        ext_host.shutdown_all(Duration::from_millis(200)).await;
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn session_tools_registers_non_colliding_tools_from_two_plugins() {
        // Sanity/regression guard for the dedup guard added alongside the
        // collision test below: two DIFFERENT plugins with DISTINCT
        // extension names must still both register — the dedup guard must
        // never reject a non-colliding full name.
        let (ctx, store, _tmp) = open_ctx().await;
        let mut host = PluginHost::new();
        let linter_body = init_ack_with_tools(r#"[{"name":"lint","description":"lint code"}]"#);
        host.add(extension_only(
            "linter-plugin",
            "Linter Plugin",
            vec![base_spec(
                "linter",
                &linter_body,
                true,
                Duration::from_millis(500),
            )],
        ));
        let formatter_body =
            init_ack_with_tools(r#"[{"name":"format","description":"format code"}]"#);
        host.add(extension_only(
            "formatter-plugin",
            "Formatter Plugin",
            vec![base_spec(
                "formatter",
                &formatter_body,
                true,
                Duration::from_millis(500),
            )],
        ));
        store
            .set_setting_raw("plugin.linter-plugin.enabled", "true")
            .await
            .unwrap();
        store
            .set_setting_raw("plugin.formatter-plugin.enabled", "true")
            .await
            .unwrap();

        let ext_host = ExtensionHost::new();
        ext_host.spawn_all(&host, &ctx).await;

        let mut bindings = ext_host.session_tools().await;
        bindings.sort_by(|a, b| a.extension_name.cmp(&b.extension_name));
        assert_eq!(
            bindings.len(),
            2,
            "two plugins with distinct extension/tool names must both register"
        );
        assert_eq!(bindings[0].extension_name, "formatter");
        assert_eq!(bindings[0].def.name, "format");
        assert_eq!(bindings[1].extension_name, "linter");
        assert_eq!(bindings[1].def.name, "lint");

        ext_host.shutdown_all(Duration::from_millis(200)).await;
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn session_tools_dedups_a_cross_plugin_full_name_collision_deterministically() {
        // Two DIFFERENT plugins each declare an `[[extension]] name =
        // "linter"` providing a tool named "lint" — `ExtensionSpec::name` is
        // only unique WITHIN one plugin's own manifest (see its doc), not
        // globally, so both format to the identical `ext__linter__lint`
        // full name (`tools::full_tool_name`). Without a dedup guard, one
        // would silently shadow the other in
        // `harness::native::tools::ToolRegistry::with_extra`'s plain
        // `BTreeMap::insert`.
        let (ctx, store, _tmp) = open_ctx().await;
        let mut host = PluginHost::new();
        // Register the alphabetically-LATER plugin id FIRST. If the winner
        // were determined by `host.add`/registration order (or by raw
        // `HashMap` iteration order) rather than the `(plugin_id, ...)` sort
        // `tool_provision_entries` now applies, this ordering would catch
        // it: the deterministic winner must still be `aaa-linter-plugin`.
        let zzz_body = init_ack_with_tools(r#"[{"name":"lint","description":"lint from zzz"}]"#);
        host.add(extension_only(
            "zzz-linter-plugin",
            "Zzz Linter Plugin",
            vec![base_spec(
                "linter",
                &zzz_body,
                true,
                Duration::from_millis(500),
            )],
        ));
        let aaa_body = init_ack_with_tools(r#"[{"name":"lint","description":"lint from aaa"}]"#);
        host.add(extension_only(
            "aaa-linter-plugin",
            "Aaa Linter Plugin",
            vec![base_spec(
                "linter",
                &aaa_body,
                true,
                Duration::from_millis(500),
            )],
        ));
        store
            .set_setting_raw("plugin.zzz-linter-plugin.enabled", "true")
            .await
            .unwrap();
        store
            .set_setting_raw("plugin.aaa-linter-plugin.enabled", "true")
            .await
            .unwrap();

        let ext_host = ExtensionHost::new();
        ext_host.spawn_all(&host, &ctx).await;

        let bindings = ext_host.session_tools().await;
        assert_eq!(
            bindings.len(),
            1,
            "a cross-plugin full-name collision must yield exactly one \
             registered tool, never two silently-shadowing ones"
        );
        assert_eq!(bindings[0].extension_name, "linter");
        assert_eq!(bindings[0].def.name, "lint");
        assert_eq!(
            bindings[0].principal.plugin_id, "aaa-linter-plugin",
            "the deterministic winner is the plugin whose id sorts first \
             (`aaa-linter-plugin` < `zzz-linter-plugin`) — regardless of \
             `host.add`/spawn call order"
        );
        assert_eq!(
            bindings[0].def.description, "lint from aaa",
            "the SURVIVING binding must be the deterministic winner's own def, \
             not the loser's"
        );

        ext_host.shutdown_all(Duration::from_millis(200)).await;
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn session_tools_skips_a_malformed_def_but_keeps_the_valid_one() {
        let (ctx, store, _tmp) = open_ctx().await;
        let mut host = PluginHost::new();
        let body = init_ack_with_tools(r#"[{"description":"missing a name"},{"name":"lint"}]"#);
        host.add(extension_only(
            "mixed-plugin",
            "Mixed Plugin",
            vec![base_spec("mixed", &body, true, Duration::from_millis(500))],
        ));
        store
            .set_setting_raw("plugin.mixed-plugin.enabled", "true")
            .await
            .unwrap();

        let ext_host = ExtensionHost::new();
        ext_host.spawn_all(&host, &ctx).await;

        let bindings = ext_host.session_tools().await;
        assert_eq!(
            bindings.len(),
            1,
            "the malformed entry must be skipped, not crash gathering"
        );
        assert_eq!(bindings[0].def.name, "lint");

        ext_host.shutdown_all(Duration::from_millis(200)).await;
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn session_tools_is_empty_when_provides_tools_is_false() {
        let (ctx, store, _tmp) = open_ctx().await;
        let mut host = PluginHost::new();
        // The extension still returns a tool def, but `provides_tools` on
        // its manifest spec is false — the host must respect the manifest
        // flag, not just the presence of a `tools` array in the ack.
        let body = init_ack_with_tools(r#"[{"name":"lint"}]"#);
        host.add(extension_only(
            "opt-out-plugin",
            "Opt Out Plugin",
            vec![base_spec(
                "optout",
                &body,
                false,
                Duration::from_millis(500),
            )],
        ));
        store
            .set_setting_raw("plugin.opt-out-plugin.enabled", "true")
            .await
            .unwrap();

        let ext_host = ExtensionHost::new();
        ext_host.spawn_all(&host, &ctx).await;

        assert!(
            ext_host.session_tools().await.is_empty(),
            "an extension that didn't declare provides_tools must contribute no tools"
        );

        ext_host.shutdown_all(Duration::from_millis(200)).await;
    }

    #[tokio::test]
    async fn session_tools_is_empty_when_nothing_was_ever_spawned() {
        let ext_host = ExtensionHost::new();
        assert!(ext_host.session_tools().await.is_empty());
    }

    // ---------- ExtensionCaller dispatch (tool/call round trip) ----------

    /// Like [`init_ack_with_tools`] but ALSO handles a second request (the
    /// `tool/call` dispatch) with `second_response_body`, mirroring
    /// `events.rs`'s own `handshake_then` helper.
    fn init_ack_then(tools_json: &str, second_response_body: &str) -> String {
        format!(
            "IFS= read -r line; id=$(printf '%s' \"$line\" | sed -n 's/.*\"id\":\\([0-9]*\\).*/\\1/p'); \
             printf '{{\"jsonrpc\":\"2.0\",\"id\":%s,\"result\":{{\"ok\":true,\"events\":[],\"tools\":{tools}}}}}\\n' \"$id\"; \
             IFS= read -r line2; id2=$(printf '%s' \"$line2\" | sed -n 's/.*\"id\":\\([0-9]*\\).*/\\1/p'); \
             {body}",
            tools = tools_json,
            body = second_response_body,
        )
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn caller_dispatches_tool_call_and_returns_the_result() {
        let (ctx, store, _tmp) = open_ctx().await;
        let mut host = PluginHost::new();
        let body = init_ack_then(
            r#"[{"name":"lint"}]"#,
            r#"printf '{"jsonrpc":"2.0","id":%s,"result":{"content":[{"type":"text","text":"0 problems"}]}}\n' "$id2""#,
        );
        host.add(extension_only(
            "linter-plugin",
            "Linter Plugin",
            vec![base_spec("linter", &body, true, Duration::from_millis(500))],
        ));
        store
            .set_setting_raw("plugin.linter-plugin.enabled", "true")
            .await
            .unwrap();

        let ext_host = ExtensionHost::new();
        ext_host.spawn_all(&host, &ctx).await;
        let bindings = ext_host.session_tools().await;
        assert_eq!(bindings.len(), 1);

        let result = bindings[0]
            .caller
            .call("lint", json!({ "path": "src/main.rs" }))
            .await
            .expect("a well-behaved extension's tool/call must succeed");
        assert_eq!(result["content"][0]["text"], "0 problems");

        ext_host.shutdown_all(Duration::from_millis(200)).await;
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn caller_returns_an_error_when_the_extension_times_out() {
        let (ctx, store, _tmp) = open_ctx().await;
        let mut host = PluginHost::new();
        // Handshakes fine (with one tool), then never answers the tool/call.
        let body = init_ack_then(r#"[{"name":"slow"}]"#, "sleep 5");
        host.add(extension_only(
            "hangs-plugin",
            "Hangs Plugin",
            vec![base_spec("hangs", &body, true, Duration::from_millis(150))],
        ));
        store
            .set_setting_raw("plugin.hangs-plugin.enabled", "true")
            .await
            .unwrap();

        let ext_host = ExtensionHost::new();
        ext_host.spawn_all(&host, &ctx).await;
        let bindings = ext_host.session_tools().await;
        assert_eq!(bindings.len(), 1);

        let start = std::time::Instant::now();
        let result = bindings[0].caller.call("slow", json!({})).await;
        let elapsed = start.elapsed();
        assert!(
            result.is_err(),
            "a timed-out tool/call must be an Err, never hang forever"
        );
        assert!(
            elapsed < Duration::from_secs(2),
            "must not wait past the extension's own timeout budget: {elapsed:?}"
        );

        ext_host.shutdown_all(Duration::from_millis(200)).await;
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn caller_returns_an_error_when_the_extension_crashes_mid_call() {
        let (ctx, store, _tmp) = open_ctx().await;
        let mut host = PluginHost::new();
        // Handshakes fine, then reads the tool/call request and exits
        // without ever responding — the transport closes mid-dispatch.
        let body = init_ack_then(r#"[{"name":"crashy"}]"#, "exit 0");
        host.add(extension_only(
            "crashes-plugin",
            "Crashes Plugin",
            vec![base_spec(
                "crashes",
                &body,
                true,
                Duration::from_millis(500),
            )],
        ));
        store
            .set_setting_raw("plugin.crashes-plugin.enabled", "true")
            .await
            .unwrap();

        let ext_host = ExtensionHost::new();
        ext_host.spawn_all(&host, &ctx).await;
        let bindings = ext_host.session_tools().await;
        assert_eq!(bindings.len(), 1);

        let result = bindings[0].caller.call("crashy", json!({})).await;
        assert!(
            result.is_err(),
            "a crashed/closed-transport tool/call must be an Err, never panic"
        );

        ext_host.shutdown_all(Duration::from_millis(200)).await;
    }
}
