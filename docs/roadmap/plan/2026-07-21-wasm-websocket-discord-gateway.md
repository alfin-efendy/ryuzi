# WASM WebSocket Capability + Discord Gateway Migration Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Migrate Discord from the native serenity gateway to a signed WASM component with no capability regression, by first adding a generic host-mediated `ryuzi:websocket` capability and a generic host gateway↔session bridge, then deleting native Discord after a real-bot smoke test.

**Architecture:** The host owns the raw TLS WebSocket (new `ryuzi:websocket@0.1.0` capability) and the component drives it and speaks the Discord gateway protocol; a generic host bridge implements the existing `Gateway` trait backed by `WasmGatewaySupervisor`, translating component gateway events into `Router`/`ControlPlane` operations and back — including approvals and slash commands — over the UNCHANGED gateway WIT via typed event payloads + a correlation map. See `docs/design/2026-07-21-wasm-websocket-discord-gateway-design.md` (the design; read it before starting — it carries the full event contract, mapping tables, and rationale).

**Tech Stack:** Rust 2021, `wasmtime` Component Model + WASI Preview 2, WIT + `wit-bindgen` 0.57, `tokio-tungstenite` (rustls), `serde`, SQLite (`rusqlite`/`deadpool-sqlite`), Tokio, Reqwest, Bun (component build).

## Global Constraints

- `ryuzi:websocket` is a NEW separate WIT package at `0.1.0`. Do NOT modify the root `ryuzi:plugin` world and do NOT change the gateway WIT (`ryuzi:gateway@0.1.0`). Existing components' `wit-api = ">=0.1.0, <0.2.0"` ranges must stay valid.
- No plugin-ID-specific host branch anywhere. The capability and the bridge are generic; all Discord specifics live only in `plugins/discord`.
- Plugins get no raw socket, unrestricted network, filesystem, env, or subprocess. The WS adapter enforces the manifest `permissions.network` allowlist on connect AND every reconnect; `wss://` (TLS) only — reject `ws://`.
- Link every host capability adapter by its FULLY-QUALIFIED interface id (e.g. `ryuzi:websocket/websocket@0.1.0`), never a short instance name (see the Task-13b fix in `crates/core/src/plugins/runtime.rs`).
- Traps/limit breaches/invalid output must never crash the daemon. WS handles are closed when the component instance is dropped.
- Bot token stays in host secret storage (`plugin.discord.token`, `secret: true`); redacted in logs.
- Native Discord deletion (Task 11) is STAGED and GATED on the user's real-bot manual smoke — do not execute it until the user confirms the smoke passed.
- TDD; `cargo fmt`; `cargo clippy -p ryuzi-core --all-targets -- -D warnings`; targeted tests. Do not hand-edit generated Cockpit bindings. `cargo test -p ryuzi-cockpit` is CI-only on Windows (tauri#13419). WATCH DISK before big builds (`cargo clean` if `.rmeta`/metadata-stub errors appear — a Phase-6 disk-full gotcha).
- Component projects follow the `plugins/mimo` + `plugins/github` template exactly (standalone `[workspace]`, `crate-type=["cdylib"]`, pure `src/logic.rs` native-tested + thin `src/guest.rs`, wit-bindgen 0.57 `generate!`, deps materialized from `crates/plugin-sdk/wit`).

---

## Unit 1 — `ryuzi:websocket@0.1.0` capability

### Task 1: Define the `ryuzi:websocket` WIT package and wire policy/linker/validation

**Files:**
- Create: `crates/plugin-sdk/wit/deps/websocket.wit`
- Modify: `crates/core/src/plugins/runtime.rs` (add `WEBSOCKET_IMPORT` const, `HostPolicy.allow_websocket`, import authorization, linker wiring, a stub `impl websocket_iface::Host`)
- Modify: `crates/core/src/plugins/capabilities/mod.rs` (declare `pub mod websocket;`), `crates/core/src/plugins/capabilities/wit_bindings.rs` if bindings are centralized there
- Create: `crates/core/src/plugins/capabilities/websocket.rs` (stub Host impl for this task; real behavior in Task 2)
- Create/extend fixture: `crates/core/tests/fixtures/component-websocket-import/` (a minimal component whose world imports `ryuzi:websocket/websocket@0.1.0`)
- Test: inline tests in `crates/core/src/plugins/runtime.rs`

**Interfaces:**
- Produces WIT `ryuzi:websocket@0.1.0` interface `websocket` with records `ws-header{name,value}`, `ws-frame{data:list<u8>,is-text:bool}`, enum `ws-state{connecting,open,closing,closed}`, variant `ws-error{invalid-request(string),rejected,disconnected,limit-exceeded(string),failed(string)}`, and funcs `connect(url,headers)->result<u64,ws-error>`, `send(handle,frame)->result<_,ws-error>`, `poll(handle)->result<list<ws-frame>,ws-error>`, `state(handle)->result<ws-state,ws-error>`, `close(handle)->result<_,ws-error>`.
- Produces `HostPolicy.allow_websocket: bool` (set `= allow_network` in `for_installed_bundle`), `const WEBSOCKET_IMPORT: &str = "ryuzi:websocket/websocket@0.1.0"`.
- Consumes the existing `CapabilityState`, `PluginCapabilityContext` (its `network_allowlist`).

- [ ] **Step 1: Write the WIT** exactly as in the design §4.1 into `crates/plugin-sdk/wit/deps/websocket.wit`. Match the kebab-case + `result`/`variant` style of the sibling `http.wit`/`oauth.wit`.

- [ ] **Step 2: Add the fixture** `crates/core/tests/fixtures/component-websocket-import/` (Cargo.toml + `src/lib.rs` + `wit/world.wit`) mirroring `component-http-import`, but its `world.wit` imports `ryuzi:websocket/websocket@0.1.0` and its `lib.rs` calls `ryuzi::websocket::websocket::state(0)` (any call that forces the import to be retained). Register it in `crates/core/tests/fixtures/build-components.sh` and `build_fixture_components_once`.

- [ ] **Step 3: Write the failing positive-instantiation test** in `runtime.rs` (mirror `instantiate_links_a_capability_import_by_its_full_interface_name`):

```rust
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn instantiate_links_the_websocket_capability_by_full_interface_id() {
    build_fixture_components_once();
    // manifest with a non-empty network allowlist → allow_websocket = true
    let bundle = installed_fixture_bundle("component-websocket-import", vec!["gateway.example"]);
    let runtime = ComponentRuntime::new().unwrap();
    let policy = HostPolicy::for_installed_bundle(&bundle);
    assert!(policy.allow_websocket);
    let compiled = runtime.compile(&bundle, policy).expect("must compile with websocket linked");
    let ctx = capability_ctx("wsimport", vec!["gateway.example"]);
    compiled.instantiate(ctx).await.expect("must instantiate with the ws adapter linked");
}
```
Also add a deny test: a manifest with EMPTY network → `allow_websocket == false` → instantiation fails with a `DeniedImport`/`InstantiationFailed` naming `ryuzi:websocket`.

- [ ] **Step 4: Run and confirm failure.** Run: `cargo test -p ryuzi-core plugins::runtime::tests::instantiate_links_the_websocket_capability_by_full_interface_id` — Expected: FAIL (no `WEBSOCKET_IMPORT`/`allow_websocket`/adapter).

- [ ] **Step 5: Implement plumbing.** Add `WEBSOCKET_IMPORT`, `HostPolicy.allow_websocket` (deny-all `false`; `for_installed_bundle` `= allow_network`), the import-authorization arm (authorized when `name == WEBSOCKET_IMPORT && policy.allow_websocket`, else `DeniedImport`), and the linker block `if allow_websocket { websocket_iface::add_to_linker_instance(&mut linker.instance(WEBSOCKET_IMPORT)?, |s| s)? }`. Generate `websocket_iface` bindings the same way `oauth_iface` is generated. In `capabilities/websocket.rs` add a STUB `impl websocket_iface::Host for CapabilityState` where every method returns `Err(ws_error::failed("not yet implemented"))` EXCEPT the type is wired so instantiation succeeds (instantiation only needs the funcs linked, not called).

- [ ] **Step 6: Run tests + fmt + clippy.** Run: `cargo test -p ryuzi-core plugins::runtime` then `cargo fmt` and `cargo clippy -p ryuzi-core --all-targets -- -D warnings`. Expected: PASS, clean.

- [ ] **Step 7: Commit.**
```bash
git add crates/plugin-sdk/wit/deps/websocket.wit crates/core/src/plugins/runtime.rs crates/core/src/plugins/capabilities/ crates/core/tests/fixtures/
git commit -m "feat(plugins): add ryuzi:websocket capability plumbing (policy, linker, validation)"
```

### Task 2: Implement the host WebSocket adapter behavior

**Files:**
- Modify: `crates/core/src/plugins/capabilities/websocket.rs` (real `connect`/`send`/`poll`/`state`/`close`)
- Modify: `crates/core/src/plugins/capabilities/mod.rs` (add per-instance connection registry to `CapabilityState`/`PluginCapabilityContext` as appropriate)
- Modify: `Cargo.toml` + `crates/core/Cargo.toml` (add `tokio-tungstenite` with rustls to `[workspace.dependencies]`, `workspace = true` in core)
- Test: inline tests in `crates/core/src/plugins/capabilities/websocket.rs` using a local `tokio-tungstenite` echo server

**Interfaces:**
- Produces a per-instance `WsRegistry` (`HashMap<u64, WsConn>` + handle counter + limits constants `MAX_WS_HANDLES_PER_INSTANCE=4`, `MAX_WS_FRAME_BYTES=1_048_576`, `MAX_WS_INBOUND_BUFFER=1024`), reachable from `CapabilityState`.
- Consumes `PluginCapabilityContext.network_allowlist` and the existing `AllowedHttpClient::host_is_allowed` matching helper (reuse it; do not re-implement allowlist matching).

- [ ] **Step 1: Add the dep.** Add `tokio-tungstenite = { version = "…", default-features = false, features = ["connect", "rustls-tls-webpki-roots"] }` (pick the current version; verify rustls feature name against its docs) to `[workspace.dependencies]`; `workspace = true` in `crates/core/Cargo.toml`. Run `cargo build -p ryuzi-core` to confirm it resolves.

- [ ] **Step 2: Write failing behavior tests** against a local echo server (spawn a `tokio-tungstenite` server on `127.0.0.1:0`, like the axum servers in `github_e2e.rs`):
  - allowlisted host (`127.0.0.1`) `connect` → `Ok(handle)`; `send` a text frame → `poll` returns the echoed frame.
  - non-allowlisted host → `connect` returns `ws-error::rejected`.
  - `ws://` scheme → `invalid-request`.
  - frame > `MAX_WS_FRAME_BYTES` on `send` → `limit-exceeded`.
  - opening a 5th handle → `limit-exceeded`.
  - after server closes, `state(handle)` → `closed` (or `poll` → `disconnected`).
Name one: `websocket_connect_send_poll_roundtrip_on_allowlisted_host`.

- [ ] **Step 3: Run + confirm failure.** `cargo test -p ryuzi-core plugins::capabilities::websocket` — Expected: FAIL (stub returns `failed`).

- [ ] **Step 4: Implement the adapter.** Follow the async-from-sync-host-call pattern in `capabilities/http.rs`/`oauth.rs` (block on the tokio handle inside the host fn). `connect`: parse URL, require `wss` (allow `ws://127.0.0.1` ONLY under `#[cfg(test)]`, or better: keep `wss`-only and have tests use a `wss` echo server with a test cert — simplest is to gate a test-only `ws` allowance behind `cfg(test)`), check host via `host_is_allowed`, open, spawn a bounded reader task filling `Mutex<VecDeque<ws-frame>>`, enforce handle cap. `send`/`poll`/`state`/`close` operate on the registry; enforce frame/buffer caps. Add a `Drop`/teardown that aborts reader tasks + closes sockets when the instance's `CapabilityState` drops.

- [ ] **Step 5: Run tests + fmt + clippy.** Expected: PASS, clean, pristine output.

- [ ] **Step 6: Commit.**
```bash
git add Cargo.toml Cargo.lock crates/core/Cargo.toml crates/core/src/plugins/capabilities/websocket.rs crates/core/src/plugins/capabilities/mod.rs
git commit -m "feat(core): host-mediated ryuzi:websocket adapter (connect/send/poll/state/close)"
```

---

## Unit 2 — Host gateway↔session bridge

### Task 3: Define the typed gateway event contract and the correlation map

**Files:**
- Create: `crates/core/src/plugins/wasm_gateway_bridge.rs` (start it here; grows over Tasks 4–6)
- Modify: `crates/core/src/plugins/mod.rs` (`pub mod wasm_gateway_bridge;`)
- Test: inline tests in `wasm_gateway_bridge.rs`

**Interfaces:**
- Produces serde types for the event contract (design §5.2/§5.3): `enum InboundEvent { MessageMention{workspace_id,actor,prompt,attachments:Vec<String>}, MessageThread{conversation_id,actor,prompt,attachments}, MessageDm{conversation_id,user_id,text}, SlashConnect{token,user_id,opts:ConnectOptsWire,role_ids:Vec<String>}, SlashEnd{token,conversation_id}, SlashStop{token,conversation_id}, SlashStatus{token}, ApprovalDecision{request_id,allow:bool,actor:String}, OpResult{op_id,result:OpResultBody} }` (tagged by `event-type`), and `enum OutboundOp { CreateChannel{op_id,name}, CreateThread{op_id,channel_id,title}, SendMessage{op_id,channel_id,text}, EditMessage{op_id,channel_id,message_id,text}, SendMessages{op_id,channel_id,chunks:Vec<String>}, ApprovalRequest{op_id:_,request_id,conversation_id,tool,summary,approver_role_ids:Vec<String>,started_by:Option<String>,timeout_ms:Option<u64>}, InteractionReply{token,text} }`. Encode/decode to/from the WIT `gateway-event{event-type,payload:list<u8>,sequence}`.
- Produces `Correlation` = `Arc<Mutex<HashMap<CorrelationKey, oneshot::Sender<CorrelationValue>>>>` with `register(key,timeout)->oneshot::Receiver`, `resolve(key,value)`, and timeout cleanup; `CorrelationKey` covers both `op_id` and `request_id` spaces.

- [ ] **Step 1: Write failing round-trip + correlation tests.** (a) every `InboundEvent`/`OutboundOp` serializes to a `gateway-event`/payload and back losslessly; (b) `Correlation::register` then `resolve` delivers the value to the receiver; (c) a registered key not resolved within its timeout yields a timeout outcome and is cleaned from the map.

- [ ] **Step 2: Run + confirm failure.** `cargo test -p ryuzi-core plugins::wasm_gateway_bridge` — Expected: FAIL (module/types absent).

- [ ] **Step 3: Implement** the serde types (match the design's `event-type` strings exactly: `message.mention`, `message.thread`, `message.dm`, `slash.connect`, `slash.end`, `slash.stop`, `slash.status`, `approval.decision`, `op.result`; outbound kinds `create-channel`, `create-thread`, `send-message`, `edit-message`, `send-messages`, `approval-request`, `interaction-reply`) and the `Correlation` map with tokio `oneshot` + a timeout wrapper.

- [ ] **Step 4: Run + fmt + clippy.** Expected: PASS, clean.

- [ ] **Step 5: Commit.**
```bash
git add crates/core/src/plugins/wasm_gateway_bridge.rs crates/core/src/plugins/mod.rs
git commit -m "feat(core): gateway bridge event contract + correlation map"
```

### Task 4: Implement `impl Gateway` (outbound) backed by the supervisor

**Files:**
- Modify: `crates/core/src/plugins/wasm_gateway_bridge.rs`
- Modify: `crates/core/src/plugins/wasm_gateway.rs` (add an immediate post-`deliver-outbound` poll to collect `op.result` promptly; expose a hook for the bridge to receive inbound events — a callback or an mpsc the bridge drains)
- Test: inline integration tests using an extended `component-gateway` fixture that consumes outbound ops and emits `op.result`

**Interfaces:**
- Produces `WasmGateway { id: String, supervisor: WasmGatewaySupervisor, router: OnceCell<Arc<Router>>, correlation: Arc<Correlation> }` implementing `crate::gateway::Gateway` (`id/start/stop/create_workspace/create_conversation/post_status/edit_status/post_result/post_error/request_approval/set_router/subscribe_status`).
- Consumes `Correlation`, the event contract (Task 3), the supervisor's `deliver_outbound`/`status`.

- [ ] **Step 1: Extend the gateway fixture** `crates/core/tests/fixtures/component-gateway/` so that on `deliver-outbound` it decodes the `OutboundOp` and, for `create-channel`/`create-thread`/`send-message`/`edit-message`/`send-messages`, queues a matching `op.result` inbound event (e.g. returns `channel_id="chan-1"`); on `approval-request` it queues an `approval.decision` (allow=true, actor="tester") after one poll. Keep the existing trap/boom behavior.

- [ ] **Step 2: Write failing tests.** Build a `WasmGateway` over the extended fixture; assert `create_workspace("proj")` returns `"chan-1"`, `create_conversation("chan-1","t")` returns a thread id, `post_status` returns a `MessageRef` whose `message_id` came from the `op.result`, `edit_status` succeeds, `post_result(chunks)` succeeds, and `request_approval(...)` returns `AllowOnce` with the actor. Name one: `wasm_gateway_create_workspace_correlates_op_result`.

- [ ] **Step 3: Run + confirm failure.**

- [ ] **Step 4: Implement `impl Gateway`.** Each outbound method: mint an `op_id`, `correlation.register(op_id, timeout)`, `supervisor.deliver_outbound(encode(op))`, await the receiver, map the `op.result` to the return type. `request_approval`: register on `request_id` (not `op_id`), deliver the `approval-request` op, await the decision (Task 6 resolves it), honor `timeout_ms` → auto-reject. `set_router` stores the router; `subscribe_status` maps the supervisor snapshot to a `GatewayStatus` subscription; `id()` returns the plugin id.

- [ ] **Step 5: Run tests + fmt + clippy.** Expected: PASS.

- [ ] **Step 6: Commit.**
```bash
git commit -am "feat(core): WasmGateway impl Gateway with outbound op correlation"
```

### Task 5: Route inbound `poll-inbound` events into the Router (message flow) + sequence dedup

**Files:**
- Modify: `crates/core/src/plugins/wasm_gateway_bridge.rs`
- Modify: `crates/core/src/plugins/wasm_gateway.rs` (deliver drained inbound events to the bridge's inbound handler instead of "status only")
- Test: inline integration tests with the extended fixture emitting `message.*` events

**Interfaces:**
- Produces `WasmGateway::dispatch_inbound(&self, event: InboundEvent, sequence: u64)` calling the stored `Router`: `message.mention`→`on_start`, `message.thread`→`on_reply`, `message.dm`→`on_dm`; `op.result`/`approval.decision`→`correlation.resolve`. Tracks `last_sequence` and drops replays (`sequence <= last_sequence`).
- Consumes `Router` (`crate::router::Router`) methods `on_start`/`on_reply`/`on_dm` (exact signatures from `crates/core/src/router.rs`).

- [ ] **Step 1: Extend the fixture** to emit, on command (e.g. keyed by the gateway config `endpoint`), a `message.mention` then a `message.thread` then a duplicate (same sequence) to prove dedup.

- [ ] **Step 2: Write failing tests** with a fake/minimal `Router` (or a real `ControlPlane` test harness — mirror existing `router.rs` tests) asserting `on_start`/`on_reply`/`on_dm` are called with the decoded fields, and that a replayed sequence is dropped (no duplicate `on_reply`).

- [ ] **Step 3: Run + confirm failure.**

- [ ] **Step 4: Implement `dispatch_inbound`** + wire the supervisor's serve loop to call it for each drained inbound event (replacing the status-only `append_inbound` path for routed kinds; keep health/status events as status). Resolve `op.result`/`approval.decision` via `correlation`.

- [ ] **Step 5: Run + fmt + clippy; commit.**
```bash
git commit -am "feat(core): route wasm gateway inbound events into the Router with sequence dedup"
```

### Task 6: Slash-command reply + approval resolution correlation; daemon wiring + migration tests

**Files:**
- Modify: `crates/core/src/plugins/wasm_gateway_bridge.rs`
- Modify: `crates/core/src/daemon.rs` (construct `WasmGateway` per enabled long-lived bundle, register in `GatewayRegistry`, `set_router`; keep native path until Task 11)
- Modify: `crates/core/src/plugins/wasm_gateway.rs` if needed for construction
- Test: inline daemon/bridge migration tests

**Interfaces:**
- Produces the slash → Router → reply flow: `slash.connect`→`on_connect(...)`→`deliver-outbound(interaction-reply{token,text})`; `slash.end`→`on_end`; `slash.stop`→`on_stop`; `slash.status`→reply only. Approval decisions resolve the `request_id` oneshot registered in Task 4.
- Consumes `Router::{on_connect,on_end,on_stop}` (exact signatures + `ConnectOpts` from `router.rs`).

- [ ] **Step 1: Write failing tests.** (a) a `slash.connect` event drives `on_connect` and produces an `interaction-reply` outbound op carrying the token + the reply text; (b) `slash.stop`/`slash.end` call the right Router method; (c) migration test: an installed+enabled generic gateway bundle is constructed and `start`ed by the daemon path, a disabled one is not; (d) no test config names Discord outside a bundle fixture (assert via the constructed registry).

- [ ] **Step 2: Run + confirm failure.**

- [ ] **Step 3: Implement** the slash reply correlation (map `ConnectOpts` from `ConnectOptsWire`), approval resolution, and the daemon wiring that builds `WasmGateway`s from enabled long-lived bundles (reuse `build_gateway_supervisors`; wrap each supervisor in a `WasmGateway`, register + `set_router`). Gate behind the same enablement check as today.

- [ ] **Step 4: Run targeted core tests + fmt + clippy.** Run: `cargo test -p ryuzi-core plugins::wasm_gateway plugins::wasm_gateway_bridge daemon`. Expected: PASS.

- [ ] **Step 5: Commit.**
```bash
git commit -am "feat(core): slash/approval correlation + daemon wiring for wasm gateways"
```

---

## Unit 3 — `plugins/discord` component

### Task 7: Scaffold `plugins/discord` + Discord gateway protocol state machine

**Files:**
- Create: `plugins/discord/` (`Cargo.toml`, `ryuzi-plugin.toml`, `wit/world.wit`, `src/lib.rs`, `src/logic.rs`, `src/guest.rs`)
- Modify: `scripts/plugins/build-first-party.ts` (add `{id:"discord", dir:"plugins/discord", crateWasmStem:"ryuzi_plugin_discord"}`)
- Test: native `#[cfg(test)]` tests in `src/logic.rs`

**Interfaces:**
- Produces the manifest (design §6.3): `id="discord"`, `lifecycle="singleton"`, `network=["gateway.discord.gg","discord.com","*.discord.com","*.discordapp.com"]`, token/app_id/guild_id settings.
- Produces pure `logic.rs` protocol state: `enum GatewayPhase`, `fn on_frame(state,&raw_json)->Vec<Action>` where `Action` is send-ws-frame / rest-call / emit-inbound-event / set-heartbeat; a `fn due_heartbeat(state, now)->Option<Frame>`. **Track Discord's gateway sequence `s`** from each DISPATCH frame (also used in heartbeat + RESUME). This `s` is the host-facing `sequence` for emitted `message.*` events (see the sequence contract below).
- **SEQUENCE CONTRACT (from the T5 review — do NOT deviate):** when emitting a `message.*` inbound `gateway-event`, set its `sequence` to Discord's gateway `s` for that DISPATCH (monotonic per connection; re-delivered unchanged on RESUME, so the host bridge dedups replays). Do NOT use a fresh return-order counter for messages. `op.result`/`approval.decision`/slash events are idempotent/correlation-keyed and carry no meaningful sequence (the host never sequence-gates them) — set `sequence` to 0 (or the current `s`) for those; it is ignored.
- Imports `ryuzi:websocket`, `ryuzi:http`, `ryuzi:settings`, `ryuzi:storage`; exports `ryuzi:gateway`.

- [ ] **Step 1: Scaffold** the crate mirroring `plugins/github` (standalone workspace, cdylib, wit-bindgen 0.57, release profile). `wit/world.wit` imports the four host interfaces + exports `ryuzi:gateway`.

- [ ] **Step 2: Write failing protocol tests** in `logic.rs` with synthetic Discord gateway JSON: HELLO(op 10, heartbeat_interval) → produces an IDENTIFY frame with the intents `GUILDS|GUILD_MESSAGES|MESSAGE_CONTENT`; after `interval` elapsed, `due_heartbeat` returns an op-1 frame with the last seq; READY(op 0, type READY) stores `bot_user_id`+`session_id`; RECONNECT(op 7)/INVALID_SESSION(op 9) → produce RESUME or re-IDENTIFY; a `closed` socket → reconnect plan. Assert exact opcodes/fields.

- [ ] **Step 3: Run + confirm failure.** `cd plugins/discord && cargo test`.

- [ ] **Step 4: Implement** the pure protocol state machine in `logic.rs` (no host calls). Keep it fully deterministic (clock passed in as a param; `guest.rs` supplies `Instant::now()` via WASI).

- [ ] **Step 5: Run native tests + build wasm.** `cd plugins/discord && cargo test`; then materialize deps + `cargo build --target wasm32-wasip2 --release` (or `bun scripts/plugins/build-first-party.ts discord` with a throwaway key). Expected: PASS, wasm builds.

- [ ] **Step 6: Commit.**
```bash
git add plugins/discord scripts/plugins/build-first-party.ts
git commit -m "feat(plugins/discord): scaffold + Discord gateway protocol state machine"
```

### Task 8: Message normalization → inbound events

**Files:**
- Modify: `plugins/discord/src/logic.rs`
- Test: native tests in `src/logic.rs`

**Interfaces:**
- Produces `fn normalize_message(raw_json, bot_user_id) -> Option<InboundEvent>` reproducing the native rules (`gateway/discord/mod.rs:255-312`): ignore `author_bot`; DM (`guild_id` absent) with non-empty content → `message.dm`; thread → `message.thread`; channel `@mention` → strip mentions → `message.mention`; else drop. `is_thread` requires a REST `GET /channels/{id}` classification (model it as a step the guest resolves; in `logic.rs` accept the channel type as an input so it stays pure).

- [ ] **Step 1: Write failing tests** covering: bot-authored dropped; DM routed; thread reply routed; channel @mention → mention with mentions stripped and empty-prompt-no-attachment dropped; non-mention channel message dropped. Match the design §5.2 event shapes.

- [ ] **Step 2: Run + confirm failure.**

- [ ] **Step 3: Implement** `normalize_message` purely (channel-type + bot id passed in).

- [ ] **Step 4: Run tests; commit.**
```bash
git commit -am "feat(plugins/discord): message normalization to inbound events"
```

### Task 9: Slash commands + approval buttons (inbound interactions)

**Files:**
- Modify: `plugins/discord/src/logic.rs`
- Test: native tests in `src/logic.rs`

**Interfaces:**
- Produces the four command defs (`/connect{name,git,model,effort,mode}`, `/end`, `/stop`, `/status` — `gateway/discord/mod.rs:520-547`) and `fn handle_interaction(raw_json) -> InteractionOutcome` where the outcome is either a `slash.*` inbound event (carrying the interaction `token`) after an immediate defer, or an approval-button click → run `can_approve(clicker_roles, approver_roles, is_starter)` (port `gateway/discord/policy.rs:105-119`) → an `approval.decision` inbound event + a REST edit, or an unauthorized ephemeral reply.

- [ ] **Step 1: Write failing tests:** each slash command JSON → the correct `slash.*` event with token+options; a `{request_id}:approve` button click by an authorized role → `approval.decision{allow:true,actor}`; unauthorized click → no decision + an ephemeral-reply action; `can_approve` truth table (starter allowed; empty approver set denies; role intersection).

- [ ] **Step 2: Run + confirm failure.**

- [ ] **Step 3: Implement** `handle_interaction` + `can_approve` purely.

- [ ] **Step 4: Run tests; commit.**
```bash
git commit -am "feat(plugins/discord): slash commands + approval-button handling"
```

### Task 10: Outbound REST + `deliver-outbound` op handling; guest wiring; wasm build

**Files:**
- Modify: `plugins/discord/src/logic.rs` (build REST requests + parse responses for each outbound op) and `src/guest.rs` (wire `ws`/`http`/`settings` imports, drive the state machine on `start`/`poll-inbound`/`deliver-outbound`/`health-check`/`stop`)
- Test: native tests in `src/logic.rs`

**Interfaces:**
- Produces `fn plan_outbound(op: OutboundOp) -> RestRequest` (create channel/thread, send/edit message, register commands, edit interaction response — Discord REST `discord.com/api/v10`, `Authorization: Bot {token}`) and `fn parse_outbound(op_kind, status, body) -> OpResultBody` yielding the `op.result` the bridge awaits. `guest.rs` maps `deliver-outbound(event)` → `plan_outbound` → `http.request` → `parse_outbound` → queue an `op.result` inbound event; `poll-inbound` drains ws frames through `on_frame`/`normalize_message`/`handle_interaction` plus any queued `op.result`s.

- [ ] **Step 1: Write failing tests** for each outbound op's request (method/url/JSON body/headers) and response parse (e.g. create-channel → `channel_id`; send-message → `message_id`). Assert the component never logs/leaks the token except as the `Bot` auth header.

- [ ] **Step 2: Run + confirm failure.**

- [ ] **Step 3: Implement** `plan_outbound`/`parse_outbound` purely; wire `guest.rs` (thin) to the ws/http/settings imports and the gateway exports. Read token/app_id/guild_id via `settings`. **CARRIED FROM T7 review (guest-wiring — do these here):** (a) on a resume-reconnect, the guest MUST reconnect to the stored `resume_gateway_url` (from READY), not the base `GATEWAY_URL` — resuming to the base can land on a server without the session and provoke INVALID_SESSION; (b) the guest drive loop MUST detect a dropped socket (`ws::state()` == `closed`/`disconnected`, or a `ws::poll` disconnect) and trigger a prompt reconnect (resume if a session exists) rather than waiting ~41s for the heartbeat-blackout path. **CARRIED FROM T8:** (c) `normalize_message` takes an `is_thread: bool` that `guest.rs` currently hardcodes `false` — the guest MUST classify the channel via a REST `GET /channels/{id}` round-trip (thread types PublicThread/PrivateThread/NewsThread → `is_thread=true`), as the native `Handler::message` does; until wired, thread replies mis-route to the mention/bare rules. Cache the channel-type lookup if practical. **CARRIED FROM T9:** (d) when the guest populates `pending_approvals` from `deliver-outbound(approval-request)` (whose payload carries `tool`), add a `tool: String` field to `PendingApproval` and restore native's approval edit-text suffix `" — **{tool}**"` in `handle_button`.

- [ ] **Step 4: Run native tests + build wasm.** `cd plugins/discord && cargo test`; `cargo build --target wasm32-wasip2 --release` (materialize deps). Expected: PASS, wasm builds, output pristine.

- [ ] **Step 5: Commit.**
```bash
git commit -am "feat(plugins/discord): outbound REST + deliver-outbound handling + guest wiring"
```

---

## Unit 4 — Delete native Discord (STAGED, GATED on user real-bot smoke)

### Task 11: Remove native Discord paths and enforce the grep gate

> **DO NOT START until the user confirms the real-bot manual smoke passed** (install the signed `discord` bundle, set token/app_id/guild_id, `/connect`, @mention start, thread reply, `/stop`, a tool approval button, reconnect after a network blip). Until then, native and WASM Discord coexist.

**Files:**
- Delete: `crates/core/src/gateway/discord/mod.rs`, `crates/core/src/gateway/discord/serenity_port.rs`, `crates/core/src/gateway/discord/policy.rs` (after porting `can_approve` into the component — done in Task 9)
- Delete: `crates/core/src/plugins/builtin.rs::discord_plugin` (relocate `native_plugin` registration if `builtin.rs` is deleted wholesale — keep native_plugin)
- Modify: `crates/core/src/daemon.rs` (remove the factory map + `enabled_gateways` CSV Discord seed + `extra_gateway_factories` Discord dependency), `crates/core/src/settings/catalog.rs` (remove `DISCORD_FIELDS` + the discord `CATALOG` entry), `crates/core/src/plugins/mod.rs`, `crates/core/src/gateway/mod.rs` (drop the discord registration), `crates/runner/Cargo.toml` (remove `discord` feature), `crates/runner/src/daemon_cmd.rs` (remove `factory_entries()`), relevant runner tests, docs (`docs/development/plugins.md`).

**Interfaces:** Daemon consumes only `WasmGateway` (via the supervisor + bridge); no Discord factory map or `enabled_gateways` CSV.

- [ ] **Step 1: Write/adjust migration tests** proving the daemon starts an installed+enabled generic gateway and that no test configuration names Discord outside the Discord bundle fixture (extend Task 6's tests).
- [ ] **Step 2: Remove** the native Discord code and wiring listed above; relocate any non-Discord responsibility out of `builtin.rs` before deleting it. Remove the `serenity` dep if now unused.
- [ ] **Step 3: Run the grep gate.** Run: `rg -n "discord_plugin|factory_entries|enabled_gateways = discord|plugin.id == \"discord\"" crates apps` — Expected: no runtime hit outside migration tests/docs.
- [ ] **Step 4: Run full core + runner tests + clippy + fmt.** Run: `cargo test -p ryuzi-core -p ryuzi-runner`, `cargo clippy -p ryuzi-core -p ryuzi-runner --all-targets -- -D warnings`, `cargo fmt --check`. Expected: green (watch disk).
- [ ] **Step 5: Commit.**
```bash
git commit -am "refactor(core): remove native Discord gateway (migrated to WASM component)"
```

## Plan self-review

- **Spec coverage:** U1 = Tasks 1–2 (WIT + adapter). U2 = Tasks 3–6 (event contract, outbound `impl Gateway`, inbound routing, slash/approval correlation + daemon wiring). U3 = Tasks 7–10 (protocol, normalization, interactions, REST + guest). U4 = Task 11 (deletion, gated). Design §4/§5/§6/§7 each map to tasks.
- **Sequencing:** Task 2 depends on Task 1 (capability plumbing). Tasks 4–6 depend on Task 3 (event contract) and use the extended fixture. Tasks 8–10 depend on Task 7 (scaffold) + Task 3 (event contract). Task 11 depends on Units 1–3 proven + the user smoke.
- **Type consistency:** the `InboundEvent`/`OutboundOp` variants + `event-type` strings are defined once in Task 3 and reused verbatim by Tasks 4–6 (host) and 7–10 (component). `can_approve` is ported in Task 9 and its native original deleted in Task 11.
- **Placeholder scan:** protocol/REST tasks specify behavior + representative tests + exact templates (mimo/github, http/oauth adapters, native discord routing rules) rather than fabricated line-by-line protocol code — the honest level for wasmtime/Discord-protocol work; each task still ends with a runnable test + commit.
- **Gated deletion:** Task 11 is explicitly blocked on the user's real-bot smoke, per the design's staged-deletion safeguard.
