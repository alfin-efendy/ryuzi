//! The Discord gateway's pure logic (this module — no `serenity`, no feature
//! gate, unconditionally compiled): a hexagonal `DiscordPort` trait,
//! slash-command JSON defs, and message/interaction routing. User-visible
//! surfaces (reply strings, mention stripping, command shapes) are exact
//! contracts covered by this module's tests; the port trait shape and the
//! reply-callback plumbing are internal design choices. The real
//! `serenity`-backed `DiscordPort` lives in [`serenity_port`] (Task 6),
//! behind the `discord` feature — see that module's doc for the Step 0
//! API-validation gate findings.
//!
//! **Delegated decision — reply callback shape:** the brief allowed swapping
//! the `dyn Fn(String) -> BoxFuture<'static, ()>` reply closure for an
//! `mpsc::Sender<String>` if the borrow checker fought the closure form. It
//! didn't: `handle_interaction`/`InboundHandlers::on_interaction` take
//! `reply: &(dyn Fn(String) -> BoxFuture<'static, ()> + Sync)` (the `futures`
//! crate is already a `ryuzi-core` dependency), matching the brief's primary
//! sketch: an async "send this text back" callback.
//!
//! **Delegated decision — constructor shape (Task 5, superseded by Task 6):**
//! Task 5 landed `DiscordGateway::new(port, router: Arc<Router>) ->
//! Arc<Self>`. Task 6 needs a real `serenity`-backed `DiscordPort`
//! (`serenity_port::SerenityDiscordPort`, behind the `discord` feature) built
//! by a `GatewayFactory` — but `build_daemon` builds every gateway via its
//! factory BEFORE the outbound `Router` exists (the `Router` itself takes
//! the already-built gateway list), so a two-argument constructor requiring
//! an `Arc<Router>` up front can't be satisfied at that point. The brief
//! offered two ways to break the cycle: an `OnceCell`/`ArcSwapOption` handed
//! to the factory, or inverting the constructor. Chose the simpler one:
//! **`DiscordGateway::new(port) -> Arc<Self>`, plus `set_router(&self,
//! Arc<Router>)`** (also added as a default no-op on the `Gateway` trait
//! itself — see `gateway::mod`'s doc), called by `build_daemon` right after
//! constructing the inbound `Router` instance. Inbound events
//! (`handle_message`/`handle_interaction`) arriving before `set_router` runs
//! are dropped with an `eprintln!` warning rather than panicking or
//! blocking — a real Discord connector only fires events after its own
//! `connect()` resolves, and `build_daemon` calls `set_router` before
//! `gw.start()`, so this is expected to be unreachable in production; it's
//! there for robustness (and is exercised directly in this module's tests).
//!
//! Internally, the router (now `RwLock<Option<Arc<Router>>>`, populated by
//! `set_router`) + gateway id are still held by a small `InboundRouting`
//! struct (`Arc`-wrapped, implementing `InboundHandlers`) rather than
//! directly on `DiscordGateway` — `Gateway::start` needs to hand
//! `DiscordPort::connect` an `Arc<dyn InboundHandlers>`, but `start(&self)`
//! only has `&self`, not `Arc<Self>`. Since routing logic never touches
//! `self.port` (only `self.router`/the gateway id), splitting it into its
//! own cheaply-`Arc`-cloneable struct sidesteps the "no `Arc<Self>` from
//! `&self`" problem entirely, without a self-referential `Weak<Self>` field.
//!
//! **Delegated decision — where the `discord` feature gate lives:** the
//! brief's daemon-wiring sketch reads `#[cfg(feature = "discord")]` in
//! `daemon_cmd.rs` (the `ryuzi-runner` crate). `#[cfg(feature = "...")]` always
//! checks the COMPILING crate's own declared features, never a dependency's
//! — and `ryuzi-runner`'s `Cargo.toml` change for this task is a hardcoded
//! feature request on its `ryuzi-core` path dependency (`features = ["otel",
//! "discord"]`), not a new `[features]` toggle on `ryuzi-runner` itself (the
//! runner has none today; `otel` isn't runner-toggleable either — see
//! `daemon.rs`'s `#[cfg(feature = "otel")]` gates, all of which live in
//! `ryuzi-core`). So a literal `#[cfg(feature = "discord")]` inside
//! `daemon_cmd.rs` would just always be `false` (unknown feature on that
//! crate), silently registering nothing. Instead, [`factory_entries`] below
//! lives HERE, in `ryuzi-core`, gated on `ryuzi-core`'s own real `discord`
//! feature; the runner calls it unconditionally. This keeps the "empty vec
//! under `not(feature)`" behavior meaningful and testable from
//! `ryuzi-core`'s own two-feature-state test suite, while `ryuzi-runner`'s
//! build (which always requests `ryuzi-core/discord`) always gets a
//! populated one.

use crate::domain::{ApprovalDecision, ApprovalRequest, AttachmentRef, PermMode, Surface};
use crate::gateway::{Gateway, GatewayFactory, MessageRef};
use crate::router::{ConnectOpts, Router};
use async_trait::async_trait;
use futures::future::BoxFuture;
use std::collections::HashMap;
use std::sync::{Arc, RwLock};

#[cfg(feature = "discord")]
pub mod serenity_port;

#[cfg(feature = "discord")]
pub use serenity_port::{DiscordGatewayFactory, SerenityDiscordPort};

/// [`crate::daemon::BuildDaemonOpts::extra_gateway_factories`] entry for
/// Discord, gated on the `discord` feature — see the module doc's "where the
/// feature gate lives" decision. Empty under `not(feature = "discord")`, so
/// `ryuzi-core`'s default test build (and anything else that doesn't ask for
/// `discord`) never touches `serenity`.
#[cfg(feature = "discord")]
pub fn factory_entries() -> Vec<(String, Arc<dyn GatewayFactory>)> {
    vec![(
        GATEWAY_ID.to_string(),
        Arc::new(DiscordGatewayFactory::new()) as Arc<dyn GatewayFactory>,
    )]
}

/// See [`factory_entries`] above (the `discord`-feature-gated version).
#[cfg(not(feature = "discord"))]
pub fn factory_entries() -> Vec<(String, Arc<dyn GatewayFactory>)> {
    Vec::new()
}

/// This gateway's stable id — matches `Surface.gateway` and the key it's
/// registered under in a `Router`'s gateway map.
const GATEWAY_ID: &str = "discord";

/// An inbound Discord message, already normalized by the connector: ids as
/// plain strings, bot-author and bot-mention flags precomputed.
#[derive(Debug, Clone)]
pub struct InboundMessage {
    pub channel_id: String,
    pub is_thread: bool,
    /// A direct message (no guild) — serenity's `Message.guild_id.is_none()`.
    /// A DM channel is never also a thread, but this is a distinct flag
    /// (not derived from `is_thread`) since the connector is the one place
    /// that can see `guild_id`.
    pub is_dm: bool,
    pub author_bot: bool,
    pub author_id: String,
    pub mentions_bot: bool,
    pub content: String,
    pub attachments: Vec<AttachmentRef>,
}

/// An inbound Discord slash-command interaction. `role_ids` is a plain
/// `Vec<String>` that defaults to empty when the member has no roles — no
/// `Option` wrapper, since "absent" and "no roles" mean the same thing here.
#[derive(Debug, Clone)]
pub struct InboundInteraction {
    pub name: String,
    pub user_id: String,
    pub channel_id: String,
    pub options: HashMap<String, String>,
    pub role_ids: Vec<String>,
}

/// The approval request shape handed to `DiscordPort::request_approval`:
/// everything a connector needs to render the approve/deny prompt and
/// enforce the approver-role gate and timeout.
#[derive(Debug, Clone)]
pub struct PortApprovalRequest {
    pub request_id: String,
    pub tool: String,
    pub summary: String,
    pub approver_role_ids: Vec<String>,
    pub started_by: Option<String>,
    pub timeout_ms: u64,
}

/// The hexagonal boundary to the real Discord connection (production:
/// `serenity`-backed [`serenity_port`]; in this module's tests, `FakePort`
/// doubles). There is deliberately no `bot_user_id()` accessor — routing
/// never needs the bot's own user id because `InboundMessage.mentions_bot`
/// is precomputed by the caller — and `disconnect` is mandatory: every real
/// implementation needs one anyway.
#[async_trait]
pub trait DiscordPort: Send + Sync {
    async fn connect(&self, handlers: Arc<dyn InboundHandlers>) -> anyhow::Result<()>;
    async fn disconnect(&self) -> anyhow::Result<()>;
    async fn create_text_channel(&self, name: &str) -> anyhow::Result<String>;
    async fn create_thread(&self, channel_id: &str, name: &str) -> anyhow::Result<String>;
    async fn send_message(&self, channel_id: &str, text: &str) -> anyhow::Result<String>;
    async fn edit_message(
        &self,
        channel_id: &str,
        message_id: &str,
        text: &str,
    ) -> anyhow::Result<()>;
    async fn request_approval(
        &self,
        conversation_id: &str,
        req: &PortApprovalRequest,
    ) -> anyhow::Result<(bool, String)>;
}

/// Handed to `DiscordPort::connect` so a real connector can dispatch inbound
/// Discord events back into gateway routing. Implemented internally (by
/// `InboundRouting`, wrapped by `DiscordGateway`); connectors only call it,
/// never implement it.
#[async_trait]
pub trait InboundHandlers: Send + Sync {
    async fn on_message(&self, e: InboundMessage);
    async fn on_interaction(
        &self,
        e: InboundInteraction,
        reply: &(dyn Fn(String) -> BoxFuture<'static, ()> + Sync),
    );
}

/// Removes user mentions from message content, then trims the result.
/// `<@!?\d+>` hand-rolled (no `regex` dependency in `ryuzi-core`): a literal
/// `<@`, an optional `!`, one-or-more ASCII digits, then `>`. Operates on
/// `char`s (not bytes) so it's correct on non-ASCII content; only ASCII
/// digits count (via `char::is_ascii_digit`). A **role** mention (`<@&id>`)
/// has `&` where a digit or `!` is required, so it never matches and is left
/// untouched — only the final trim can affect it.
fn strip_mentions(content: &str) -> String {
    let chars: Vec<char> = content.chars().collect();
    let n = chars.len();
    let mut out = String::with_capacity(content.len());
    let mut i = 0;
    while i < n {
        if chars[i] == '<' && i + 1 < n && chars[i + 1] == '@' {
            let mut j = i + 2;
            if j < n && chars[j] == '!' {
                j += 1;
            }
            let digits_start = j;
            while j < n && chars[j].is_ascii_digit() {
                j += 1;
            }
            if j > digits_start && j < n && chars[j] == '>' {
                i = j + 1;
                continue;
            }
        }
        out.push(chars[i]);
        i += 1;
    }
    out.trim().to_string()
}

/// The routing logic shared by `DiscordGateway`'s directly-callable
/// `handle_message`/`handle_interaction` methods and its `InboundHandlers`
/// impl handed to `DiscordPort::connect` — see the module doc for why this
/// is a separate `Arc`-held struct rather than living directly on
/// `DiscordGateway`.
///
/// `router` starts `None` (Task 6's `set_router` inversion — see the module
/// doc): a `DiscordGateway` can exist before `build_daemon` has a `Router` to
/// give it. `RwLock` (not `Mutex`) since reads (every inbound event) vastly
/// outnumber the single `set_router` write.
struct InboundRouting {
    router: RwLock<Option<Arc<Router>>>,
}

impl InboundRouting {
    fn router(&self) -> Option<Arc<Router>> {
        self.router.read().unwrap().clone()
    }

    /// Routes an inbound message: bot authors are ignored; a DM (no
    /// `/connect`, no project binding) starts or continues a project-less
    /// `chat` session; a thread message with content or attachments becomes
    /// a reply into the session bound to that thread; a channel message
    /// that mentions the bot starts a new turn (mentions stripped first).
    /// Events arriving before `set_router` are dropped with a warning (see
    /// the module doc).
    async fn handle_message(&self, e: InboundMessage) {
        let Some(router) = self.router() else {
            eprintln!("[discord] dropping inbound message: router not set yet");
            return;
        };
        if e.author_bot {
            return;
        }
        if e.is_dm {
            // `on_dm` has no attachments parameter today (A7 scope: text-only
            // project-less chat) — an attachment-only DM with empty text is
            // silently dropped rather than starting a session with a blank
            // prompt.
            if !e.content.is_empty() {
                if let Err(err) = router
                    .on_dm(GATEWAY_ID, &e.channel_id, &e.author_id, &e.content)
                    .await
                {
                    eprintln!("[discord] on_dm failed: {err}");
                }
            }
            return;
        }
        if e.is_thread {
            if !e.content.is_empty() || !e.attachments.is_empty() {
                if let Err(err) = router
                    .on_reply(
                        GATEWAY_ID,
                        &e.channel_id,
                        &e.author_id,
                        &e.content,
                        &e.attachments,
                    )
                    .await
                {
                    eprintln!("[discord] on_reply failed: {err}");
                }
            }
            return;
        }
        if e.mentions_bot {
            let prompt = strip_mentions(&e.content);
            if !prompt.is_empty() || !e.attachments.is_empty() {
                if let Err(err) = router
                    .on_start(
                        GATEWAY_ID,
                        &e.channel_id,
                        &e.author_id,
                        &prompt,
                        &e.attachments,
                    )
                    .await
                {
                    eprintln!("[discord] on_start failed: {err}");
                }
            }
        }
    }

    /// Routes a slash-command interaction (`/connect`, `/end`, `/stop`,
    /// `/status`) and replies with the outcome. Any error is caught and sent
    /// back as an `❌ {err}` reply instead of propagating. Events arriving
    /// before `set_router` are dropped with a warning (see the module doc).
    async fn handle_interaction(
        &self,
        e: InboundInteraction,
        reply: &(dyn Fn(String) -> BoxFuture<'static, ()> + Sync),
    ) {
        let Some(router) = self.router() else {
            eprintln!("[discord] dropping inbound interaction: router not set yet");
            return;
        };
        let result: anyhow::Result<()> = async {
            match e.name.as_str() {
                "connect" => {
                    let opts = ConnectOpts {
                        name: e.options.get("name").cloned(),
                        git_url: e.options.get("git").cloned(),
                        settings: crate::control::ProvisionSettings {
                            model: e.options.get("model").cloned(),
                            effort: e.options.get("effort").cloned(),
                            perm_mode: e.options.get("mode").map(|m| PermMode::from_db(m)),
                        },
                        actor_role_ids: e.role_ids.clone(),
                    };
                    let outcome = router.on_connect(GATEWAY_ID, &e.user_id, opts).await?;
                    let mut msg = format!("✅ connected → <#{}>", outcome.workspace_id);
                    if outcome.perm_mode_downgraded {
                        msg.push_str(
                            "\n⚠️ bypassPermissions requires an admin role — using default mode.",
                        );
                    }
                    reply(msg).await;
                }
                "end" => {
                    router.on_end(GATEWAY_ID, &e.channel_id).await?;
                    reply("🟥 session ended".to_string()).await;
                }
                "stop" => {
                    router.on_stop(GATEWAY_ID, &e.channel_id).await?;
                    reply("⏹️ stopping the current turn".to_string()).await;
                }
                "status" => {
                    reply("harness is running ✅".to_string()).await;
                }
                _ => {}
            }
            Ok(())
        }
        .await;
        if let Err(err) = result {
            reply(format!("❌ {err}")).await;
        }
    }
}

#[async_trait]
impl InboundHandlers for InboundRouting {
    async fn on_message(&self, e: InboundMessage) {
        self.handle_message(e).await;
    }
    async fn on_interaction(
        &self,
        e: InboundInteraction,
        reply: &(dyn Fn(String) -> BoxFuture<'static, ()> + Sync),
    ) {
        self.handle_interaction(e, reply).await;
    }
}

/// The Discord `Gateway` implementation: renders core output over a
/// `DiscordPort` and routes inbound messages/interactions through a
/// `Router`.
pub struct DiscordGateway {
    port: Arc<dyn DiscordPort>,
    inbound: Arc<InboundRouting>,
}

impl DiscordGateway {
    /// No `Router` yet (Task 6's `set_router` inversion — see the module
    /// doc): inbound events are dropped-with-a-warning until [`Self::set_router`]
    /// (also reachable via the `Gateway` trait) is called.
    pub fn new(port: Arc<dyn DiscordPort>) -> Arc<Self> {
        Arc::new(DiscordGateway {
            port,
            inbound: Arc::new(InboundRouting {
                router: RwLock::new(None),
            }),
        })
    }

    /// Directly callable so tests can inject inbound messages without a
    /// connected port.
    pub async fn handle_message(&self, e: InboundMessage) {
        self.inbound.handle_message(e).await;
    }

    /// Directly callable so tests can inject interactions without a
    /// connected port.
    pub async fn handle_interaction(
        &self,
        e: InboundInteraction,
        reply: &(dyn Fn(String) -> BoxFuture<'static, ()> + Sync),
    ) {
        self.inbound.handle_interaction(e, reply).await;
    }
}

#[async_trait]
impl Gateway for DiscordGateway {
    fn id(&self) -> &str {
        GATEWAY_ID
    }

    async fn start(&self) -> anyhow::Result<()> {
        self.port
            .connect(self.inbound.clone() as Arc<dyn InboundHandlers>)
            .await
    }

    async fn stop(&self) -> anyhow::Result<()> {
        self.port.disconnect().await
    }

    fn set_router(&self, router: Arc<Router>) {
        *self.inbound.router.write().unwrap() = Some(router);
    }

    async fn create_workspace(&self, name: &str) -> anyhow::Result<String> {
        self.port.create_text_channel(name).await
    }

    async fn create_conversation(&self, workspace_id: &str, title: &str) -> anyhow::Result<String> {
        let truncated: String = title.chars().take(90).collect();
        let truncated = if truncated.is_empty() {
            "session".to_string()
        } else {
            truncated
        };
        self.port.create_thread(workspace_id, &truncated).await
    }

    async fn post_status(&self, surface: &Surface, text: &str) -> anyhow::Result<MessageRef> {
        let message_id = self
            .port
            .send_message(&surface.conversation_id, text)
            .await?;
        Ok(MessageRef {
            surface: surface.clone(),
            message_id,
        })
    }

    async fn edit_status(&self, msg: &MessageRef, text: &str) -> anyhow::Result<()> {
        self.port
            .edit_message(&msg.surface.conversation_id, &msg.message_id, text)
            .await
    }

    async fn post_result(&self, surface: &Surface, chunks: &[String]) -> anyhow::Result<()> {
        for c in chunks {
            self.port.send_message(&surface.conversation_id, c).await?;
        }
        Ok(())
    }

    async fn post_error(&self, surface: &Surface, text: &str) -> anyhow::Result<()> {
        self.port
            .send_message(&surface.conversation_id, &format!("❌ {text}"))
            .await?;
        Ok(())
    }

    async fn request_approval(
        &self,
        surface: &Surface,
        req: &ApprovalRequest,
    ) -> anyhow::Result<ApprovalDecision> {
        let port_req = PortApprovalRequest {
            request_id: req.request_id.clone(),
            tool: req.tool.clone(),
            summary: req.summary.clone(),
            approver_role_ids: req.approver_role_ids.clone(),
            started_by: req.started_by.clone(),
            timeout_ms: req.timeout_ms.unwrap_or(300_000),
        };
        let (allow, _actor) = self
            .port
            .request_approval(&surface.conversation_id, &port_req)
            .await?;
        Ok(if allow {
            ApprovalDecision::AllowOnce
        } else {
            ApprovalDecision::RejectOnce
        })
    }
}

/// Plain Discord application-command JSON (a valid REST body; no `serenity`
/// import). Option `"type": 3` is Discord's
/// `ApplicationCommandOptionType.String`, inlined as the literal `3`.
pub fn build_commands() -> serde_json::Value {
    serde_json::json!([
        {
            "name": "connect",
            "description": "Connect a repo (new folder by name, or clone a git URL) to a new channel",
            "options": [
                { "name": "name", "description": "New project folder name", "type": 3, "required": false },
                { "name": "git", "description": "Git URL to clone", "type": 3, "required": false },
                { "name": "model", "description": "Model override", "type": 3, "required": false },
                { "name": "effort", "description": "Reasoning effort", "type": 3, "required": false },
                {
                    "name": "mode",
                    "description": "Permission mode",
                    "type": 3,
                    "required": false,
                    "choices": [
                        { "name": "default", "value": "default" },
                        { "name": "acceptEdits", "value": "acceptEdits" },
                        { "name": "bypassPermissions", "value": "bypassPermissions" }
                    ]
                }
            ]
        },
        { "name": "end", "description": "End the session in this thread (removes its worktree)" },
        { "name": "stop", "description": "Stop the running turn in this thread" },
        { "name": "status", "description": "Show harness status" }
    ])
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::attachments::{AttachmentFetcher, FetchOutcome};
    use crate::control::ControlPlane;
    use crate::domain::{SessionKind, SessionStatus};
    use crate::harness::{Harness, HarnessFactory, HarnessSession, SessionCtx, TurnPrompt};
    use crate::plugins::Registries;
    use crate::settings::SettingsStore;
    use crate::store::Store;
    use crate::telemetry::NoopTelemetry;
    use serial_test::serial;
    use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
    use std::sync::Mutex;

    // ---------- stripMentions ----------

    #[test]
    fn strip_mentions_removes_user_mention_variants() {
        assert_eq!(strip_mentions("<@123> hello"), "hello");
        assert_eq!(strip_mentions("<@!456> hi"), "hi");
    }

    #[test]
    fn strip_mentions_preserves_role_mentions() {
        assert_eq!(strip_mentions("<@&789> keep"), "<@&789> keep");
    }

    #[test]
    fn strip_mentions_trims_result() {
        assert_eq!(strip_mentions("  <@1>   "), "");
        assert_eq!(strip_mentions("  hi  "), "hi");
    }

    // ---------- build_commands ----------

    #[test]
    fn build_commands_defines_connect_end_stop_status() {
        let names: Vec<String> = build_commands()
            .as_array()
            .unwrap()
            .iter()
            .map(|c| c["name"].as_str().unwrap().to_string())
            .collect();
        for expected in ["connect", "end", "stop", "status"] {
            assert!(
                names.contains(&expected.to_string()),
                "missing {expected}: {names:?}"
            );
        }
    }

    #[test]
    fn build_commands_connect_has_expected_options_and_mode_choice_order() {
        let commands = build_commands();
        let connect = commands
            .as_array()
            .unwrap()
            .iter()
            .find(|c| c["name"] == "connect")
            .unwrap();
        let opts = connect["options"].as_array().unwrap();
        let opt_names: Vec<String> = opts
            .iter()
            .map(|o| o["name"].as_str().unwrap().to_string())
            .collect();
        for expected in ["name", "git", "model", "effort", "mode"] {
            assert!(
                opt_names.contains(&expected.to_string()),
                "missing {expected}: {opt_names:?}"
            );
        }
        let mode = opts.iter().find(|o| o["name"] == "mode").unwrap();
        let values: Vec<String> = mode["choices"]
            .as_array()
            .unwrap()
            .iter()
            .map(|c| c["value"].as_str().unwrap().to_string())
            .collect();
        assert_eq!(values, vec!["default", "acceptEdits", "bypassPermissions"]);
    }

    // ---------- FakePort ----------

    /// Records every call as `verb:args` and hands back incrementing
    /// `chan-N`/`thread-N`/`msg-N` ids so tests can assert exact call
    /// sequences.
    struct FakePort {
        calls: Mutex<Vec<String>>,
        connected: AtomicBool,
        n: AtomicU64,
        last_approval: Mutex<Option<PortApprovalRequest>>,
    }

    impl FakePort {
        fn new() -> Self {
            FakePort {
                calls: Mutex::new(Vec::new()),
                connected: AtomicBool::new(false),
                n: AtomicU64::new(0),
                last_approval: Mutex::new(None),
            }
        }
        fn calls(&self) -> Vec<String> {
            self.calls.lock().unwrap().clone()
        }
        fn connected(&self) -> bool {
            self.connected.load(Ordering::SeqCst)
        }
        fn last_approval(&self) -> Option<PortApprovalRequest> {
            self.last_approval.lock().unwrap().clone()
        }
    }

    #[async_trait]
    impl DiscordPort for FakePort {
        async fn connect(&self, _handlers: Arc<dyn InboundHandlers>) -> anyhow::Result<()> {
            self.connected.store(true, Ordering::SeqCst);
            Ok(())
        }
        async fn disconnect(&self) -> anyhow::Result<()> {
            Ok(())
        }
        async fn create_text_channel(&self, name: &str) -> anyhow::Result<String> {
            self.calls
                .lock()
                .unwrap()
                .push(format!("createTextChannel:{name}"));
            let n = self.n.fetch_add(1, Ordering::SeqCst) + 1;
            Ok(format!("chan-{n}"))
        }
        async fn create_thread(&self, channel_id: &str, name: &str) -> anyhow::Result<String> {
            self.calls
                .lock()
                .unwrap()
                .push(format!("createThread:{channel_id}:{name}"));
            let n = self.n.fetch_add(1, Ordering::SeqCst) + 1;
            Ok(format!("thread-{n}"))
        }
        async fn send_message(&self, channel_id: &str, text: &str) -> anyhow::Result<String> {
            self.calls
                .lock()
                .unwrap()
                .push(format!("send:{channel_id}:{text}"));
            let n = self.n.fetch_add(1, Ordering::SeqCst) + 1;
            Ok(format!("msg-{n}"))
        }
        async fn edit_message(
            &self,
            channel_id: &str,
            message_id: &str,
            text: &str,
        ) -> anyhow::Result<()> {
            self.calls
                .lock()
                .unwrap()
                .push(format!("edit:{channel_id}:{message_id}:{text}"));
            Ok(())
        }
        async fn request_approval(
            &self,
            conversation_id: &str,
            req: &PortApprovalRequest,
        ) -> anyhow::Result<(bool, String)> {
            self.calls
                .lock()
                .unwrap()
                .push(format!("requestApproval:{conversation_id}"));
            *self.last_approval.lock().unwrap() = Some(req.clone());
            Ok((false, "u9".to_string()))
        }
    }

    fn base_msg() -> InboundMessage {
        InboundMessage {
            channel_id: "c".to_string(),
            is_thread: false,
            is_dm: false,
            author_bot: false,
            author_id: "u".to_string(),
            mentions_bot: false,
            content: String::new(),
            attachments: vec![],
        }
    }

    async fn minimal_cp() -> Arc<ControlPlane> {
        let tmp = tempfile::NamedTempFile::new().unwrap();
        let store = Store::open(tmp.path()).await.unwrap();
        ControlPlane::new(store, Registries::new()).await
    }

    // ---------- Test 1: output methods delegate to the port ----------

    #[tokio::test]
    async fn output_methods_delegate_to_the_port() {
        let port = Arc::new(FakePort::new());
        let router = Arc::new(Router::new(minimal_cp().await, vec![]));
        let gw = DiscordGateway::new(port.clone());
        gw.set_router(router);

        let ws = gw.create_workspace("foo").await.unwrap();
        let conv = gw.create_conversation(&ws, "title").await.unwrap();
        let target = Surface {
            gateway: "discord".to_string(),
            conversation_id: conv.clone(),
        };
        let msg_ref = gw.post_status(&target, "working").await.unwrap();
        gw.edit_status(&msg_ref, "done").await.unwrap();
        gw.post_result(&target, &["a".to_string(), "b".to_string()])
            .await
            .unwrap();

        assert_eq!(
            port.calls(),
            vec![
                "createTextChannel:foo".to_string(),
                "createThread:chan-1:title".to_string(),
                "send:thread-2:working".to_string(),
                "edit:thread-2:msg-3:done".to_string(),
                "send:thread-2:a".to_string(),
                "send:thread-2:b".to_string(),
            ]
        );
    }

    // ---------- Test 4: start() connects the port ----------

    #[tokio::test]
    async fn start_connects_the_port() {
        let port = Arc::new(FakePort::new());
        let router = Arc::new(Router::new(minimal_cp().await, vec![]));
        let gw = DiscordGateway::new(port.clone());
        gw.set_router(router);
        Gateway::start(&*gw).await.unwrap();
        assert!(port.connected());
    }

    // ---------- Test 5: requestApproval forwards to the port ----------

    #[tokio::test]
    async fn request_approval_forwards_to_the_port_and_returns_its_decision() {
        let port = Arc::new(FakePort::new());
        let router = Arc::new(Router::new(minimal_cp().await, vec![]));
        let gw = DiscordGateway::new(port.clone());
        gw.set_router(router);

        let dec = gw
            .request_approval(
                &Surface {
                    gateway: "discord".to_string(),
                    conversation_id: "t1".to_string(),
                },
                &ApprovalRequest {
                    request_id: "r1".to_string(),
                    tool: "Bash".to_string(),
                    summary: "Bash: rm".to_string(),
                    approver_role_ids: vec!["r1".to_string()],
                    started_by: Some("u1".to_string()),
                    timeout_ms: Some(1000),
                },
            )
            .await
            .unwrap();

        assert_eq!(dec, ApprovalDecision::RejectOnce);
        assert!(port.calls().contains(&"requestApproval:t1".to_string()));
        assert_eq!(
            port.last_approval().unwrap().approver_role_ids,
            vec!["r1".to_string()]
        );
    }

    // ---------- Tests 2, 3, 6: message/interaction routing over a real Router ----------
    //
    // `Router` (unlike `DiscordPort`) is a concrete, DB-backed type, not an
    // injectable interface — so these tests can't just swap in a recording
    // router stub. Instead they wire a real `Router` to a real
    // `ControlPlane`/`Store` (tempdir-backed, exactly `router.rs`'s own
    // inbound-Router test pattern) with a `FakeGateway` registered under
    // "discord" — DELIBERATELY NOT the `DiscordGateway` under test itself,
    // decoupling "what DiscordGateway does with its own FakePort" from "what
    // Router does with ITS registered gateway when DiscordGateway's routing
    // calls into it". Router-side effects (a `FakeGateway.create_workspace`/
    // `create_conversation` call, a bound project/session showing up in the
    // `Store`) are the observable outcomes these tests assert on.

    struct StateDirGuard {
        _dir: tempfile::TempDir,
    }

    impl StateDirGuard {
        fn new() -> Self {
            let dir = tempfile::tempdir().expect("tempdir");
            std::env::set_var("XDG_DATA_HOME", dir.path().join("data"));
            std::env::set_var("HOME", dir.path());
            std::fs::write(
                dir.path().join(".gitconfig"),
                "[user]\n\tname = Test\n\temail = test@example.com\n",
            )
            .expect("write .gitconfig");
            StateDirGuard { _dir: dir }
        }
    }

    struct OneShotSession;
    #[async_trait]
    impl HarnessSession for OneShotSession {
        async fn send_prompt(&self, _prompt: TurnPrompt) -> anyhow::Result<()> {
            Ok(())
        }
        async fn cancel(&self) -> anyhow::Result<()> {
            Ok(())
        }
        async fn end(&self) -> anyhow::Result<()> {
            Ok(())
        }
        fn agent_session_id(&self) -> Option<String> {
            None
        }
    }
    struct OneShotHarness;
    #[async_trait]
    impl Harness for OneShotHarness {
        async fn start_session(&self, _ctx: SessionCtx) -> anyhow::Result<Box<dyn HarnessSession>> {
            Ok(Box::new(OneShotSession))
        }
    }
    struct OneShotHarnessFactory;
    impl HarnessFactory for OneShotHarnessFactory {
        fn create(&self) -> anyhow::Result<Arc<dyn Harness>> {
            Ok(Arc::new(OneShotHarness))
        }
    }

    /// Never actually reached over the network: `prepare_attachments` only
    /// calls the fetcher for a non-empty attachment list, and this test
    /// module never asserts on saved/skipped outcomes — only that routing
    /// with attachments present doesn't hang or panic.
    struct StubFetcher;
    impl AttachmentFetcher for StubFetcher {
        fn fetch_capped(&self, _url: &str, _max_bytes: u64) -> anyhow::Result<FetchOutcome> {
            Ok(FetchOutcome::HttpError(404))
        }
    }

    /// A recording `Gateway` registered under "discord" in the test `Router`
    /// — separate from the `DiscordGateway` under test. It observes the
    /// `Router`'s OWN collaborator boundary rather than replacing the
    /// `Router` itself (see the block comment above).
    struct FakeGateway {
        calls: Mutex<Vec<String>>,
        n: AtomicU64,
    }
    impl FakeGateway {
        fn new() -> Self {
            FakeGateway {
                calls: Mutex::new(Vec::new()),
                n: AtomicU64::new(0),
            }
        }
        fn calls(&self) -> Vec<String> {
            self.calls.lock().unwrap().clone()
        }
    }
    #[async_trait]
    impl Gateway for FakeGateway {
        fn id(&self) -> &str {
            "discord"
        }
        async fn start(&self) -> anyhow::Result<()> {
            Ok(())
        }
        async fn stop(&self) -> anyhow::Result<()> {
            Ok(())
        }
        async fn create_workspace(&self, name: &str) -> anyhow::Result<String> {
            self.calls
                .lock()
                .unwrap()
                .push(format!("create_workspace:{name}"));
            Ok(format!("ws-{name}"))
        }
        async fn create_conversation(
            &self,
            workspace_id: &str,
            title: &str,
        ) -> anyhow::Result<String> {
            let n = self.n.fetch_add(1, Ordering::SeqCst);
            self.calls
                .lock()
                .unwrap()
                .push(format!("create_conversation:{workspace_id}:{title}"));
            Ok(format!("conv-{n}"))
        }
        async fn post_status(&self, surface: &Surface, _text: &str) -> anyhow::Result<MessageRef> {
            Ok(MessageRef {
                surface: surface.clone(),
                message_id: "m".to_string(),
            })
        }
        async fn edit_status(&self, _msg: &MessageRef, _text: &str) -> anyhow::Result<()> {
            Ok(())
        }
        async fn post_result(&self, _surface: &Surface, _chunks: &[String]) -> anyhow::Result<()> {
            Ok(())
        }
        async fn post_error(&self, _surface: &Surface, _text: &str) -> anyhow::Result<()> {
            Ok(())
        }
        async fn request_approval(
            &self,
            _surface: &Surface,
            _req: &ApprovalRequest,
        ) -> anyhow::Result<ApprovalDecision> {
            Ok(ApprovalDecision::AllowOnce)
        }
    }

    /// A `Router` wired to a fresh `ControlPlane`/`Store` (workdir_root set,
    /// "native" harness registered — the default runtime `connect_project`
    /// resolves to since Plan C — a fake attachment fetcher so
    /// `prepare_attachments` never touches the network) with `FakeGateway`
    /// registered under "discord". Mirrors `router.rs`'s own
    /// `wired_control_plane` test helper.
    async fn wired_router(
        root: &std::path::Path,
    ) -> (
        Arc<Router>,
        Arc<FakeGateway>,
        Arc<Store>,
        tempfile::NamedTempFile,
    ) {
        let db_guard = tempfile::NamedTempFile::new().unwrap();
        let store = Store::open(db_guard.path()).await.unwrap();
        let mut regs = Registries::new();
        regs.harness = Arc::new(OneShotHarnessFactory);
        let cp = ControlPlane::new_full(
            Arc::new(store),
            regs,
            Arc::new(NoopTelemetry),
            Arc::new(StubFetcher),
        )
        .await;
        let store_ref = cp.store().clone();
        SettingsStore::new(store_ref.clone())
            .set("workdir_root", root.to_str().unwrap())
            .await
            .unwrap();
        let fake_gw = Arc::new(FakeGateway::new());
        let router = Arc::new(Router::new(cp, vec![fake_gw.clone() as Arc<dyn Gateway>]));
        (router, fake_gw, store_ref, db_guard)
    }

    /// Poll a session's status until it matches `status` (or panic) —
    /// `spawn_prompt`'s completion (and the resulting Running→Idle demotion)
    /// runs in a detached `tokio::spawn`, so this is needed to deterministically
    /// observe a turn having finished instead of racing it. Mirrors
    /// `router.rs`'s own `wait_for_status` test helper.
    async fn wait_for_status(store: &Store, pk: &str, status: SessionStatus) {
        for _ in 0..300 {
            if let Some(s) = store.get_session(pk).await.unwrap() {
                if s.status == status {
                    return;
                }
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
        panic!("timed out waiting for status {status:?}");
    }

    // ---------- Test 2: ignores bot messages; thread→reply; mention→start; else ignore ----------

    #[tokio::test]
    #[serial]
    async fn ignores_bot_messages_thread_replies_mention_starts_else_ignored() {
        let _guard = StateDirGuard::new();
        let root = tempfile::tempdir().unwrap();
        let (router, fake_gw, store, _db_guard) = wired_router(root.path()).await;
        let port = Arc::new(FakePort::new());
        let gw = DiscordGateway::new(port);
        gw.set_router(router.clone());

        let outcome = router
            .on_connect(
                "discord",
                "u1",
                ConnectOpts {
                    name: Some("proj".to_string()),
                    ..Default::default()
                },
            )
            .await
            .unwrap();
        let ws_id = outcome.workspace_id.clone();

        // A non-thread mention starts a session; the mention is stripped from the prompt.
        gw.handle_message(InboundMessage {
            channel_id: ws_id.clone(),
            author_id: "u".to_string(),
            mentions_bot: true,
            content: "<@12345> do it".to_string(),
            ..base_msg()
        })
        .await;
        assert!(
            fake_gw
                .calls()
                .contains(&format!("create_conversation:{ws_id}:do it")),
            "expected a stripped-mention conversation title, got: {:?}",
            fake_gw.calls()
        );
        let sessions = store.list_sessions(None).await.unwrap();
        assert_eq!(sessions.len(), 1);
        let session_pk = sessions[0].session_pk.clone();
        wait_for_status(&store, &session_pk, SessionStatus::Idle).await;
        let conv_id = store.surfaces(&session_pk).await.unwrap()[0]
            .conversation_id
            .clone();

        // Bot messages are dropped outright, even in a thread with content —
        // and even when targeting THIS ALREADY-REGISTERED conversation, where
        // (unlike an unmapped id) `on_reply` would actually resolve a session
        // and drive a second turn if the `author_bot` guard were deleted.
        // `fake_gw.calls()` can't observe that (`on_reply` never touches the
        // gateway), so the proxy is `last_active`: it's only ever bumped by
        // `demote_if_running`, which only runs once a real turn completes.
        let baseline = store.get_session(&session_pk).await.unwrap().unwrap();
        assert_eq!(baseline.status, SessionStatus::Idle);
        let last_active_before = baseline.last_active;
        gw.handle_message(InboundMessage {
            channel_id: conv_id.clone(),
            is_thread: true,
            author_bot: true,
            author_id: "u".to_string(),
            content: "x".to_string(),
            ..base_msg()
        })
        .await;
        // Grace period: give a wrongly-fired turn's background completion
        // (were the guard deleted) a chance to land before asserting nothing
        // moved. The fake harness's turn is an immediate no-op, so this
        // window is ample even under a loaded CI runner.
        for _ in 0..30 {
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
        let after_bot_msg = store.get_session(&session_pk).await.unwrap().unwrap();
        assert_eq!(
            after_bot_msg.status,
            SessionStatus::Idle,
            "authorBot message must not leave the session Running"
        );
        assert_eq!(
            after_bot_msg.last_active, last_active_before,
            "authorBot message must not continue the session (last_active moved, implying a second turn ran)"
        );

        // A (non-bot) thread message continues that same session.
        gw.handle_message(InboundMessage {
            channel_id: conv_id,
            is_thread: true,
            author_id: "u".to_string(),
            content: "more".to_string(),
            ..base_msg()
        })
        .await;
        wait_for_status(&store, &session_pk, SessionStatus::Idle).await;

        // Neither a thread nor a mention: ignored.
        gw.handle_message(InboundMessage {
            channel_id: "unrelated".to_string(),
            author_id: "u".to_string(),
            content: "just chatting".to_string(),
            ..base_msg()
        })
        .await;
        assert_eq!(store.list_sessions(None).await.unwrap().len(), 1);
    }

    // ---------- Test 3: interaction connect routes to on_connect and replies with the channel ----------

    #[tokio::test]
    #[serial]
    async fn interaction_connect_routes_to_on_connect_and_replies_with_the_channel() {
        let _guard = StateDirGuard::new();
        let root = tempfile::tempdir().unwrap();
        let (router, fake_gw, _store, _db_guard) = wired_router(root.path()).await;
        let port = Arc::new(FakePort::new());
        let gw = DiscordGateway::new(port);
        gw.set_router(router);

        let mut options = HashMap::new();
        options.insert("name".to_string(), "foo".to_string());
        let interaction = InboundInteraction {
            name: "connect".to_string(),
            user_id: "u".to_string(),
            channel_id: "c".to_string(),
            options,
            role_ids: vec![],
        };

        let replies = Arc::new(Mutex::new(Vec::<String>::new()));
        let sink = replies.clone();
        let reply: Box<dyn Fn(String) -> BoxFuture<'static, ()> + Sync> = Box::new(move |text| {
            let sink = sink.clone();
            Box::pin(async move {
                sink.lock().unwrap().push(text);
            })
        });

        gw.handle_interaction(interaction, reply.as_ref()).await;

        assert!(
            fake_gw
                .calls()
                .contains(&"create_workspace:foo".to_string()),
            "expected create_workspace:foo, got: {:?}",
            fake_gw.calls()
        );
        let replies = replies.lock().unwrap();
        assert_eq!(replies.len(), 1);
        assert!(replies[0].contains("ws-foo"), "reply was: {}", replies[0]);
    }

    // ---------- Test 6: attachment-only messages start/reply even with empty text ----------

    #[tokio::test]
    #[serial]
    async fn attachment_only_messages_start_and_reply_even_with_empty_text() {
        let _guard = StateDirGuard::new();
        let root = tempfile::tempdir().unwrap();
        let (router, fake_gw, store, _db_guard) = wired_router(root.path()).await;
        let port = Arc::new(FakePort::new());
        let gw = DiscordGateway::new(port);
        gw.set_router(router.clone());

        let outcome = router
            .on_connect(
                "discord",
                "u1",
                ConnectOpts {
                    name: Some("proj2".to_string()),
                    ..Default::default()
                },
            )
            .await
            .unwrap();
        let ws_id = outcome.workspace_id.clone();

        let att = AttachmentRef {
            name: "a.png".to_string(),
            url: "https://cdn/a".to_string(),
            content_type: Some("image/png".to_string()),
            size: 10,
        };

        // A bare mention (prompt strips to empty) plus an attachment still starts.
        gw.handle_message(InboundMessage {
            channel_id: ws_id.clone(),
            author_id: "u".to_string(),
            mentions_bot: true,
            content: "<@1>".to_string(),
            attachments: vec![att.clone()],
            ..base_msg()
        })
        .await;
        assert!(
            fake_gw
                .calls()
                .contains(&format!("create_conversation:{ws_id}:session")),
            "expected the empty-prompt \"session\" fallback title, got: {:?}",
            fake_gw.calls()
        );
        let sessions = store.list_sessions(None).await.unwrap();
        assert_eq!(sessions.len(), 1);
        let session_pk = sessions[0].session_pk.clone();
        wait_for_status(&store, &session_pk, SessionStatus::Idle).await;
        let conv_id = store.surfaces(&session_pk).await.unwrap()[0]
            .conversation_id
            .clone();

        // Empty content plus an attachment still replies.
        gw.handle_message(InboundMessage {
            channel_id: conv_id,
            is_thread: true,
            author_id: "u".to_string(),
            attachments: vec![att.clone()],
            ..base_msg()
        })
        .await;
        wait_for_status(&store, &session_pk, SessionStatus::Idle).await;

        // Empty content AND no attachments: ignored — targeting the SAME
        // already-connected workspace (not an unmapped one), so `on_start`
        // WOULD run (a project is bound to `ws_id`) if the
        // `!prompt.is_empty() || !attachments.is_empty()` guard were removed.
        // The session count staying unchanged is what proves the guard, not
        // an unrelated "workspace never connected" no-op.
        gw.handle_message(InboundMessage {
            channel_id: ws_id.clone(),
            author_id: "u".to_string(),
            mentions_bot: true,
            content: "<@1>".to_string(),
            ..base_msg()
        })
        .await;
        assert_eq!(store.list_sessions(None).await.unwrap().len(), 1);
    }

    // ---------- Task A7: DM messages start/continue a project-less chat session ----------

    /// A DM (`is_dm: true`) with no `/connect`/workspace binding starts a
    /// project-less `chat` session bound to the DM channel, and a second DM
    /// in the same channel continues it — proving the DM path bypasses
    /// `resolve_project_by_workspace` entirely (unlike a guild mention,
    /// which requires `/connect` first). It also never touches the
    /// registered gateway (`create_workspace`/`create_conversation`),
    /// unlike `on_start`.
    #[tokio::test]
    #[serial]
    async fn dm_message_starts_a_project_less_chat_session() {
        let _guard = StateDirGuard::new();
        let root = tempfile::tempdir().unwrap();
        let (router, fake_gw, store, _db_guard) = wired_router(root.path()).await;
        let port = Arc::new(FakePort::new());
        let gw = DiscordGateway::new(port);
        gw.set_router(router.clone());

        gw.handle_message(InboundMessage {
            channel_id: "dm-1".to_string(),
            author_id: "u9".to_string(),
            is_dm: true,
            content: "hello".to_string(),
            ..base_msg()
        })
        .await;

        let session = store
            .resolve_by_conversation("discord", "dm-1")
            .await
            .unwrap()
            .expect("expected a chat session bound to the DM");
        assert_eq!(session.kind, SessionKind::Chat);
        assert_eq!(session.project_id, None);
        wait_for_status(&store, &session.session_pk, SessionStatus::Idle).await;

        // A second DM in the same channel continues the same session.
        gw.handle_message(InboundMessage {
            channel_id: "dm-1".to_string(),
            author_id: "u9".to_string(),
            is_dm: true,
            content: "again".to_string(),
            ..base_msg()
        })
        .await;
        assert_eq!(store.list_sessions(None).await.unwrap().len(), 1);
        wait_for_status(&store, &session.session_pk, SessionStatus::Idle).await;

        assert!(
            fake_gw.calls().is_empty(),
            "DM chat sessions must not touch the gateway: {:?}",
            fake_gw.calls()
        );
    }

    /// A DM with no content and no attachments is silently ignored — `on_dm`
    /// has no attachments parameter, so there's nothing worth starting a
    /// session over.
    #[tokio::test]
    #[serial]
    async fn dm_with_empty_content_is_ignored() {
        let _guard = StateDirGuard::new();
        let root = tempfile::tempdir().unwrap();
        let (router, _fake_gw, store, _db_guard) = wired_router(root.path()).await;
        let port = Arc::new(FakePort::new());
        let gw = DiscordGateway::new(port);
        gw.set_router(router.clone());

        gw.handle_message(InboundMessage {
            channel_id: "dm-2".to_string(),
            author_id: "u9".to_string(),
            is_dm: true,
            ..base_msg()
        })
        .await;

        assert!(store.list_sessions(None).await.unwrap().is_empty());
    }

    // ---------- Test 7 (Task 6): set_router reconciliation ----------

    /// Task 6's `new(port)` + `set_router` inversion: inbound routing on a
    /// gateway that hasn't had `set_router` called yet must be dropped, not
    /// routed — then, after `set_router`, the identical event must reach the
    /// router normally. Targets an ALREADY-CONNECTED workspace for both
    /// halves (not an unmapped id `on_start` would no-op on regardless of
    /// the `set_router` guard — see the Task 5 fix report above on why a
    /// vacuous target defeats this kind of test) so the "before" assertion
    /// is only true because of the guard, not an unrelated no-op.
    #[tokio::test]
    #[serial]
    async fn inbound_events_before_set_router_are_dropped_then_routed_after() {
        let _guard = StateDirGuard::new();
        let root = tempfile::tempdir().unwrap();
        let (router, fake_gw, _store, _db_guard) = wired_router(root.path()).await;

        // Connect a real workspace up front via a separate, already-wired
        // gateway instance, so it's a genuine bound target below.
        let setup_gw = DiscordGateway::new(Arc::new(FakePort::new()));
        setup_gw.set_router(router.clone());
        let outcome = router
            .on_connect(
                "discord",
                "u1",
                ConnectOpts {
                    name: Some("proj3".to_string()),
                    ..Default::default()
                },
            )
            .await
            .unwrap();
        let ws_id = outcome.workspace_id;

        // A FRESH gateway instance, deliberately never `set_router`'d.
        let gw = DiscordGateway::new(Arc::new(FakePort::new()));
        gw.handle_message(InboundMessage {
            channel_id: ws_id.clone(),
            author_id: "u".to_string(),
            mentions_bot: true,
            content: "<@1> do it".to_string(),
            ..base_msg()
        })
        .await;
        assert!(
            !fake_gw
                .calls()
                .iter()
                .any(|c| c.starts_with("create_conversation:")),
            "inbound routing before set_router must be dropped, not routed: {:?}",
            fake_gw.calls()
        );

        gw.set_router(router.clone());
        gw.handle_message(InboundMessage {
            channel_id: ws_id.clone(),
            author_id: "u".to_string(),
            mentions_bot: true,
            content: "<@1> do it".to_string(),
            ..base_msg()
        })
        .await;
        assert!(
            fake_gw
                .calls()
                .iter()
                .any(|c| c.starts_with(&format!("create_conversation:{ws_id}:"))),
            "inbound routing after set_router must reach the router: {:?}",
            fake_gw.calls()
        );
    }
}
