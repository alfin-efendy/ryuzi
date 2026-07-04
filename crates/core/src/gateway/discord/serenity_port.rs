//! The real `serenity`-backed [`DiscordPort`] (Task 6), plus the
//! [`DiscordGatewayFactory`] that builds a `DiscordGateway` over it from a
//! flat, dotted-key config `serde_json::Value`
//! (`{"discord.token", "discord.app_id", "discord.guild_id"}` — matches
//! `build_daemon`'s catalog-driven gateway config, `settings::catalog::CATALOG`'s
//! `DISCORD_FIELDS`). Byte-exact behavioral spec: LIVE TS
//! `packages/core/src/gateways/discord/client-port.ts`'s `DiscordClientPort`
//! (+ `registerCommands`).
//!
//! **Step 0 API-validation gate (recorded):** `serenity = "0.12"` resolved to
//! `0.12.5`, `default-features = false, features = ["client", "gateway",
//! "rustls_backend", "model", "http", "builder", "collector"]` (exactly the
//! brief's starting list — no additions needed). Every construct the gate
//! asked for compiles and behaves as expected, verified both by reading
//! `serenity-0.12.5`'s source directly (registry checkout) and by this file
//! compiling end to end:
//! - `Http::new(token: &str)` — auto-prefixes `"Bot "` if missing; building
//!   the guild-command-registration `Http` also needs
//!   `Http::set_application_id` (`GuildId::set_commands` → `Http::try_application_id()`,
//!   which errors `ApplicationIdMissing` if unset — not mentioned in the
//!   brief's notes, found by reading the source).
//! - `GuildId::new(u64)` / `ChannelId::new(u64)` / `ApplicationId::new(u64)`
//!   / `MessageId::new(u64)` **panic on a zero id** (all four share one
//!   macro) — [`parse_nonzero_id`]/[`channel_id_from_str`] guard against this
//!   so a malformed config or bad conversation id is a returned `Err`/tuple,
//!   never a panic.
//! - `CreateCommand::new(name).description(..).add_option(CreateCommandOption::new(kind,
//!   name, description).required(..).add_string_choice(..))` — matches the
//!   brief's sketch; [`commands_from_json`] converts `build_commands()`'s
//!   JSON into these builders.
//! - `CreateThread::new(name)`, `CreateButton::new(id).label(..).style(ButtonStyle::Success)`
//!   — exactly as the brief described.
//! - `Client::builder(token, intents).event_handler(handler)` is an
//!   `IntoFuture` (`.await` resolves to `Result<Client>`), not a
//!   synchronous builder — `.build()`/a separate `.start()`-only step
//!   doesn't exist; matches the brief's "future" framing.
//! - `ChannelId`/`GuildId` expose `send_message`/`edit_message`/`create_thread`/`create_channel`
//!   directly (each just forwards to the builder's `execute`) — no need to
//!   fetch a `Channel`/`GuildChannel` object first the way the TS port does
//!   (discord.js requires a fetched `Channel` object; serenity's REST
//!   builders work off bare ids). Deviation (disclosed): this means this
//!   port's `edit_message`/`send_message`/`create_thread`/`create_text_channel`
//!   let a missing-channel HTTP error propagate as an `anyhow::Error`
//!   instead of TS's silent `if (!channel) return;` — only `request_approval`
//!   has an explicit, brief-mandated "missing channel" tuple return, so only
//!   that method does an explicit existence check first.
//! - `EventHandler::{ready, message, interaction_create}` take `(&self, ctx:
//!   Context, ..data)` (macro-generated; default no-op bodies, so only the
//!   three actually used are overridden here).
//! - `ComponentInteraction{member: Option<Member>, user: User, data.custom_id}` —
//!   matches; `Member.roles: Vec<RoleId>`.
//! - `ComponentInteractionCollector::new(&shard).message_id(id).timeout(dur).stream()`
//!   returns a `Stream` (not a callback-based collector like TS's
//!   `createMessageComponentCollector`) that ends when the timeout elapses —
//!   consumed via a `.next().await` loop instead of TS's `collector.on("collect",
//!   ..)`/`.on("end", ..)` pair; behaviorally equivalent (see
//!   `SerenityDiscordPort::request_approval`'s doc).
//!
//! **Disclosed design choice — `is_thread` needs an HTTP round-trip per
//! inbound message:** TS's `msg.channel.isThread()` is free because
//! discord.js already maintains a channel cache. This port's minimal feature
//! set omits `cache` (not in the brief's starting list), so there is no local
//! channel-type cache to consult; `message()` calls `ctx.http.get_channel(..)`
//! and checks the returned `GuildChannel.kind` against the three thread
//! `ChannelType`s. This trades one extra REST call per inbound message
//! (cheap, well within Discord's rate limits for this bot's expected traffic)
//! for not adding the `cache` feature and its own complexity (population
//! races, non-`Send` guard types) to a first pass. If message volume ever
//! makes this a real cost, enabling `cache` and reading `ctx.cache.channel(..)`
//! synchronously is the natural follow-up.

use crate::domain::AttachmentRef;
use crate::gateway::discord::{
    build_commands, DiscordGateway, DiscordPort, InboundHandlers, InboundInteraction,
    InboundMessage, PortApprovalRequest,
};
use crate::gateway::{Gateway, GatewayFactory};
use crate::policy::can_approve;
use anyhow::Context as _;
use async_trait::async_trait;
use futures::future::BoxFuture;
use futures::StreamExt as _;
use serenity::all::*;
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::sync::oneshot;

/// Parse a config string into a non-zero `u64` (every serenity id type
/// panics on `0` — see the module doc). `what` names the field, for a
/// legible error.
fn parse_nonzero_id(s: &str, what: &str) -> anyhow::Result<u64> {
    let n: u64 = s
        .trim()
        .parse()
        .with_context(|| format!("invalid {what}: {s:?}"))?;
    if n == 0 {
        anyhow::bail!("invalid {what}: must be a non-zero snowflake");
    }
    Ok(n)
}

/// `None` on a non-numeric or zero id — used for `DiscordPort` method
/// arguments (channel/message ids threaded through as plain `&str`), where
/// an invalid id should surface as a normal error/tuple, never a panic.
fn channel_id_from_str(s: &str) -> Option<ChannelId> {
    s.trim()
        .parse::<u64>()
        .ok()
        .filter(|&n| n != 0)
        .map(ChannelId::new)
}

fn message_id_from_str(s: &str) -> Option<MessageId> {
    s.trim()
        .parse::<u64>()
        .ok()
        .filter(|&n| n != 0)
        .map(MessageId::new)
}

/// `build_commands()` → `Vec<CreateCommand>`. TS parity: `registerCommands`'s
/// `body: buildCommands()` payload, rebuilt through serenity's typed
/// builders instead of a raw JSON body.
fn commands_from_json(defs: &serde_json::Value) -> Vec<CreateCommand> {
    defs.as_array()
        .into_iter()
        .flatten()
        .map(command_from_json)
        .collect()
}

fn command_from_json(cmd: &serde_json::Value) -> CreateCommand {
    let name = cmd["name"].as_str().unwrap_or_default();
    let description = cmd["description"].as_str().unwrap_or_default();
    let mut builder = CreateCommand::new(name).description(description);
    if let Some(options) = cmd["options"].as_array() {
        for opt in options {
            builder = builder.add_option(command_option_from_json(opt));
        }
    }
    builder
}

/// `build_commands()` only ever emits STRING (`type: 3`) options today (see
/// `gateway::discord::build_commands`'s doc), so this hardcodes
/// `CommandOptionType::String` rather than mapping the numeric `type` field
/// generically.
fn command_option_from_json(opt: &serde_json::Value) -> CreateCommandOption {
    let name = opt["name"].as_str().unwrap_or_default();
    let description = opt["description"].as_str().unwrap_or_default();
    let required = opt["required"].as_bool().unwrap_or(false);
    let mut builder =
        CreateCommandOption::new(CommandOptionType::String, name, description).required(required);
    if let Some(choices) = opt["choices"].as_array() {
        for choice in choices {
            let cname = choice["name"].as_str().unwrap_or_default();
            let cvalue = choice["value"].as_str().unwrap_or_default();
            builder = builder.add_string_choice(cname, cvalue);
        }
    }
    builder
}

/// Populated by [`Handler::ready`], read by [`SerenityDiscordPort::request_approval`]
/// (the `ShardMessenger` needed to build a `ComponentInteractionCollector`)
/// and by [`Handler::message`] (the bot's own id, for `mentions_bot`).
struct ConnectedState {
    shard: ShardMessenger,
    bot_user_id: UserId,
}

/// The `serenity::client::EventHandler` that turns gateway events into
/// [`InboundHandlers`] calls. TS parity: the `this.client.on(Events.MessageCreate,
/// ..)` / `Events.InteractionCreate` listeners in `DiscordClientPort.connect`.
struct Handler {
    handlers: Arc<dyn InboundHandlers>,
    connected: Arc<Mutex<Option<ConnectedState>>>,
    ready_tx: Mutex<Option<oneshot::Sender<()>>>,
}

#[async_trait]
impl EventHandler for Handler {
    /// TS parity: the `Events.ClientReady` half of `connect`'s `new
    /// Promise<void>((resolve) => { this.client.once(Events.ClientReady, ()
    /// => resolve()); ... })`. Also stashes the `ShardMessenger` + this
    /// bot's own user id for later use (`request_approval`'s collector,
    /// `message`'s `mentions_bot`) — TS gets both from `this.client`
    /// directly since discord.js keeps the live client around; serenity's
    /// `Client::start` takes ownership, so this is captured off the one
    /// `Context` a handler receives instead.
    async fn ready(&self, ctx: Context, data_about_bot: Ready) {
        *self.connected.lock().unwrap() = Some(ConnectedState {
            shard: ctx.shard.clone(),
            bot_user_id: data_about_bot.user.id,
        });
        if let Some(tx) = self.ready_tx.lock().unwrap().take() {
            let _ = tx.send(());
        }
    }

    /// TS parity: `Events.MessageCreate` → `handlers.onMessage(..)`.
    async fn message(&self, ctx: Context, new_message: Message) {
        let bot_user_id = self
            .connected
            .lock()
            .unwrap()
            .as_ref()
            .map(|c| c.bot_user_id);
        let mentions_bot =
            bot_user_id.is_some_and(|id| new_message.mentions.iter().any(|u| u.id == id));
        // See the module doc: no `cache` feature, so this is an explicit
        // fetch-and-inspect rather than a free cache lookup (TS:
        // `msg.channel.isThread()`).
        let is_thread = match ctx.http.get_channel(new_message.channel_id).await {
            Ok(Channel::Guild(gc)) => matches!(
                gc.kind,
                ChannelType::PublicThread | ChannelType::PrivateThread | ChannelType::NewsThread
            ),
            _ => false,
        };
        let attachments = new_message
            .attachments
            .iter()
            .map(|a| AttachmentRef {
                name: a.filename.clone(),
                url: a.url.clone(),
                content_type: a.content_type.clone(),
                size: u64::from(a.size),
            })
            .collect();
        self.handlers
            .on_message(InboundMessage {
                channel_id: new_message.channel_id.to_string(),
                is_thread,
                author_bot: new_message.author.bot,
                author_id: new_message.author.id.to_string(),
                mentions_bot,
                content: new_message.content.clone(),
                attachments,
            })
            .await;
    }

    /// TS parity: `Events.InteractionCreate` → (chat-input commands only) →
    /// `interaction.deferReply({flags: Ephemeral})` → `handlers.onInteraction(..)`
    /// with a `reply` closure that `interaction.editReply(text)`s.
    async fn interaction_create(&self, ctx: Context, interaction: Interaction) {
        let Interaction::Command(cmd) = interaction else {
            return;
        };
        if let Err(e) = cmd.defer_ephemeral(&ctx.http).await {
            eprintln!("[discord] defer_ephemeral failed: {e}");
            return;
        }

        let mut options = HashMap::new();
        for opt in &cmd.data.options {
            if let CommandDataOptionValue::String(s) = &opt.value {
                options.insert(opt.name.clone(), s.clone());
            }
        }
        let role_ids = cmd
            .member
            .as_ref()
            .map(|m| m.roles.iter().map(|r| r.to_string()).collect())
            .unwrap_or_default();
        let inbound = InboundInteraction {
            name: cmd.data.name.clone(),
            user_id: cmd.user.id.to_string(),
            channel_id: cmd.channel_id.to_string(),
            options,
            role_ids,
        };

        let http = ctx.http.clone();
        let cmd = Arc::new(cmd);
        // `+ Send` here (not just the `+ Sync` `InboundHandlers::on_interaction`
        // requires) so THIS closure value stays `Send` — needed because it's
        // held across the `.await` below, inside a function `#[async_trait]`
        // requires to return a `Send` future. `&(dyn Fn(..) + Send + Sync)`
        // coerces to `&(dyn Fn(..) + Sync)` at the call site (dropping an
        // auto-trait bound from a reference is a standard, implicit
        // coercion), so this doesn't need any change to the trait itself.
        let reply: Box<dyn Fn(String) -> BoxFuture<'static, ()> + Send + Sync> =
            Box::new(move |text| {
                let http = http.clone();
                let cmd = cmd.clone();
                Box::pin(async move {
                    if let Err(e) = cmd
                        .edit_response(&http, EditInteractionResponse::new().content(text))
                        .await
                    {
                        eprintln!("[discord] edit_response failed: {e}");
                    }
                })
            });
        self.handlers.on_interaction(inbound, reply.as_ref()).await;
    }
}

/// The real Discord connector: a `serenity` `Client` (gateway, spawned on
/// its own task) + a persistent `Http` (used for every REST call, including
/// command registration — see the module doc's `Http::new` note). TS
/// parity: `DiscordClientPort`.
pub struct SerenityDiscordPort {
    token: String,
    guild_id: GuildId,
    http: Arc<Http>,
    connected: Arc<Mutex<Option<ConnectedState>>>,
    shard_manager: Mutex<Option<Arc<ShardManager>>>,
}

impl SerenityDiscordPort {
    pub fn new(token: String, app_id: String, guild_id: String) -> anyhow::Result<Self> {
        if token.trim().is_empty() {
            anyhow::bail!("discord.token is required");
        }
        let app_id = parse_nonzero_id(&app_id, "discord.app_id")?;
        let guild_id = parse_nonzero_id(&guild_id, "discord.guild_id")?;

        let http = Http::new(&token);
        http.set_application_id(ApplicationId::new(app_id));

        Ok(SerenityDiscordPort {
            token,
            guild_id: GuildId::new(guild_id),
            http: Arc::new(http),
            connected: Arc::new(Mutex::new(None)),
            shard_manager: Mutex::new(None),
        })
    }
}

#[async_trait]
impl DiscordPort for SerenityDiscordPort {
    /// TS parity: `connect` — `registerCommands` (guild application
    /// commands, via `Http` + `GuildId::set_commands`) BEFORE the gateway
    /// login, then resolves once `ready` fires.
    async fn connect(&self, handlers: Arc<dyn InboundHandlers>) -> anyhow::Result<()> {
        let commands = commands_from_json(&build_commands());
        self.guild_id
            .set_commands(&self.http, commands)
            .await
            .context("failed to register discord guild commands")?;

        let (ready_tx, ready_rx) = oneshot::channel();
        let handler = Handler {
            handlers,
            connected: Arc::clone(&self.connected),
            ready_tx: Mutex::new(Some(ready_tx)),
        };
        let intents = GatewayIntents::GUILDS
            | GatewayIntents::GUILD_MESSAGES
            | GatewayIntents::MESSAGE_CONTENT;
        let mut client = Client::builder(&self.token, intents)
            .event_handler(handler)
            .await
            .context("failed to build the discord gateway client")?;

        *self.shard_manager.lock().unwrap() = Some(Arc::clone(&client.shard_manager));

        tokio::spawn(async move {
            if let Err(e) = client.start().await {
                eprintln!("[discord] gateway client stopped: {e}");
            }
        });

        ready_rx
            .await
            .context("discord gateway disconnected before it became ready")?;
        Ok(())
    }

    async fn disconnect(&self) -> anyhow::Result<()> {
        // Bound in its own `let` (not `if let self.shard_manager.lock()...`)
        // so the `MutexGuard` temporary drops at the end of THIS statement —
        // an `if let` scrutinee's temporary lives for the whole block, which
        // would hold the (non-`Send`) guard across the `.await` below.
        let shard_manager = self.shard_manager.lock().unwrap().take();
        if let Some(sm) = shard_manager {
            sm.shutdown_all().await;
        }
        Ok(())
    }

    async fn create_text_channel(&self, name: &str) -> anyhow::Result<String> {
        let channel = self
            .guild_id
            .create_channel(&self.http, CreateChannel::new(name).kind(ChannelType::Text))
            .await?;
        Ok(channel.id.to_string())
    }

    async fn create_thread(&self, channel_id: &str, name: &str) -> anyhow::Result<String> {
        let cid = channel_id_from_str(channel_id)
            .with_context(|| format!("invalid channel id: {channel_id:?}"))?;
        let thread = cid
            .create_thread(&self.http, CreateThread::new(name))
            .await?;
        Ok(thread.id.to_string())
    }

    async fn send_message(&self, channel_id: &str, text: &str) -> anyhow::Result<String> {
        let cid = channel_id_from_str(channel_id)
            .with_context(|| format!("invalid channel id: {channel_id:?}"))?;
        let message = cid
            .send_message(&self.http, CreateMessage::new().content(text))
            .await?;
        Ok(message.id.to_string())
    }

    async fn edit_message(
        &self,
        channel_id: &str,
        message_id: &str,
        text: &str,
    ) -> anyhow::Result<()> {
        let cid = channel_id_from_str(channel_id)
            .with_context(|| format!("invalid channel id: {channel_id:?}"))?;
        let mid = message_id_from_str(message_id)
            .with_context(|| format!("invalid message id: {message_id:?}"))?;
        cid.edit_message(&self.http, mid, EditMessage::new().content(text))
            .await?;
        Ok(())
    }

    /// TS parity: `requestApproval` — send the two buttons, then collect
    /// component interactions against a total timeout. Rust/serenity shape:
    /// TS's callback-based `collector.on("collect", ..)`/`.on("end", ..)`
    /// pair becomes a `Stream::next()` loop (`ComponentInteractionCollector`
    /// is a `Stream`, not an event emitter) — `stream.next() == None` is the
    /// `"end"` case (timeout elapsed with no settled decision); each `Some`
    /// item is one `"collect"` event. Because this loop processes items
    /// strictly one at a time (unlike JS's single-threaded-but-reentrant
    /// event dispatch), there's no need for TS's `settled` guard flag —
    /// returning ends the loop and drops the stream (which unregisters the
    /// collector), so no later item can ever be processed after a decision.
    /// The decision is computed and ready to return BEFORE the (fallible,
    /// swallowed-error) `UpdateMessage` edit, matching the brief's "decision
    /// locked before the edit" requirement — just via straight-line
    /// sequencing rather than TS's resolve-before-await-the-edit ordering.
    async fn request_approval(
        &self,
        conversation_id: &str,
        req: &PortApprovalRequest,
    ) -> anyhow::Result<(bool, String)> {
        let Some(channel_id) = channel_id_from_str(conversation_id) else {
            return Ok((false, "no-channel".to_string()));
        };
        if self.http.get_channel(channel_id).await.is_err() {
            return Ok((false, "no-channel".to_string()));
        }

        let content = format!("🔐 Approve **{}**?\n```\n{}\n```", req.tool, req.summary);
        let components = vec![CreateActionRow::Buttons(vec![
            CreateButton::new(format!("{}:approve", req.request_id))
                .label("Approve")
                .style(ButtonStyle::Success),
            CreateButton::new(format!("{}:deny", req.request_id))
                .label("Deny")
                .style(ButtonStyle::Danger),
        ])];
        let sent = channel_id
            .send_message(
                &self.http,
                CreateMessage::new().content(content).components(components),
            )
            .await?;

        let shard = self
            .connected
            .lock()
            .unwrap()
            .as_ref()
            .map(|c| c.shard.clone());
        let shard = shard.context("discord port not connected")?;

        let mut stream = ComponentInteractionCollector::new(&shard)
            .message_id(sent.id)
            .timeout(Duration::from_millis(req.timeout_ms))
            .stream();

        loop {
            let Some(interaction) = stream.next().await else {
                return Ok((false, "timeout".to_string()));
            };

            let clicker_role_ids: Vec<String> = interaction
                .member
                .as_ref()
                .map(|m| m.roles.iter().map(|r| r.to_string()).collect())
                .unwrap_or_default();
            let actor = interaction.user.id.to_string();
            let is_starter = req.started_by.as_deref() == Some(actor.as_str());
            if !can_approve(&clicker_role_ids, &req.approver_role_ids, is_starter) {
                let _ = interaction
                    .create_response(
                        &self.http,
                        CreateInteractionResponse::Message(
                            CreateInteractionResponseMessage::new()
                                .ephemeral(true)
                                .content("You are not authorized to approve this."),
                        ),
                    )
                    .await;
                continue;
            }

            let decision = if interaction.data.custom_id.ends_with(":approve") {
                true
            } else if interaction.data.custom_id.ends_with(":deny") {
                false
            } else {
                // Unexpected customId — ignore (fail-closed; a timeout
                // denies if nothing valid ever arrives). TS parity: `if
                // (decision === null) return;` inside the collect handler.
                continue;
            };

            let label = if decision {
                "✅ Approved"
            } else {
                "🚫 Denied"
            };
            let _ = interaction
                .create_response(
                    &self.http,
                    CreateInteractionResponse::UpdateMessage(
                        CreateInteractionResponseMessage::new()
                            .content(format!("{label} by <@{actor}> — **{}**", req.tool))
                            .components(vec![]),
                    ),
                )
                .await;
            return Ok((decision, actor));
        }
    }
}

/// Builds a [`SerenityDiscordPort`]-backed [`DiscordGateway`] from a flat,
/// dotted-key config object. TS parity: the production `DiscordClientPort`
/// construction site (`apps/daemon`'s gateway wiring) — no direct TS
/// factory-object counterpart exists (TS's provider catalog wires gateways
/// imperatively), so this shape is Rust-native, matching `GatewayFactory`.
#[derive(Default)]
pub struct DiscordGatewayFactory;

impl DiscordGatewayFactory {
    pub fn new() -> Self {
        DiscordGatewayFactory
    }
}

impl GatewayFactory for DiscordGatewayFactory {
    fn create(&self, config: &serde_json::Value) -> anyhow::Result<Arc<dyn Gateway>> {
        let token = config
            .get("discord.token")
            .and_then(|v| v.as_str())
            .unwrap_or("");
        if token.trim().is_empty() {
            anyhow::bail!("discord gateway requires a non-empty discord.token");
        }
        let app_id = config
            .get("discord.app_id")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        let guild_id = config
            .get("discord.guild_id")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();

        let port = Arc::new(SerenityDiscordPort::new(
            token.to_string(),
            app_id,
            guild_id,
        )?);
        Ok(DiscordGateway::new(port) as Arc<dyn Gateway>)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `Arc<dyn Gateway>` isn't `Debug`, so `Result::unwrap_err` (which
    /// requires the `Ok` type to be `Debug`, to format it if called on an
    /// `Ok`) doesn't work directly on `DiscordGatewayFactory::create`'s
    /// return type.
    fn expect_err(r: anyhow::Result<Arc<dyn Gateway>>) -> anyhow::Error {
        match r {
            Ok(_) => panic!("expected an error"),
            Err(e) => e,
        }
    }

    // ---------- Step 1: build_commands → CreateCommand conversion ----------

    #[test]
    fn commands_from_json_preserves_names_and_option_names() {
        let commands = commands_from_json(&build_commands());
        let as_json: Vec<serde_json::Value> = commands
            .iter()
            .map(|c| serde_json::to_value(c).unwrap())
            .collect();

        let names: Vec<&str> = as_json
            .iter()
            .map(|c| c["name"].as_str().unwrap())
            .collect();
        assert_eq!(names, vec!["connect", "end", "stop", "status"]);

        let connect = as_json.iter().find(|c| c["name"] == "connect").unwrap();
        let opt_names: Vec<&str> = connect["options"]
            .as_array()
            .unwrap()
            .iter()
            .map(|o| o["name"].as_str().unwrap())
            .collect();
        assert_eq!(opt_names, vec!["name", "git", "model", "effort", "mode"]);

        let mode = connect["options"]
            .as_array()
            .unwrap()
            .iter()
            .find(|o| o["name"] == "mode")
            .unwrap();
        let choice_values: Vec<&str> = mode["choices"]
            .as_array()
            .unwrap()
            .iter()
            .map(|c| c["value"].as_str().unwrap())
            .collect();
        assert_eq!(
            choice_values,
            vec!["default", "acceptEdits", "bypassPermissions"]
        );
    }

    // ---------- Step 1: DiscordGatewayFactory ----------

    #[test]
    fn factory_create_with_a_full_config_builds_a_discord_gateway() {
        let factory = DiscordGatewayFactory::new();
        let gw = factory
            .create(&serde_json::json!({
                "discord.token": "t",
                "discord.app_id": "1",
                "discord.guild_id": "2",
            }))
            .unwrap();
        assert_eq!(gw.id(), "discord");
    }

    #[test]
    fn factory_create_missing_token_is_an_error() {
        let factory = DiscordGatewayFactory::new();
        let err = expect_err(factory.create(&serde_json::json!({
            "discord.app_id": "1",
            "discord.guild_id": "2",
        })));
        assert!(
            err.to_string().contains("discord.token"),
            "error should mention discord.token: {err}"
        );
    }

    #[test]
    fn factory_create_empty_token_is_an_error() {
        let factory = DiscordGatewayFactory::new();
        let err = expect_err(factory.create(&serde_json::json!({
            "discord.token": "",
            "discord.app_id": "1",
            "discord.guild_id": "2",
        })));
        assert!(
            err.to_string().contains("discord.token"),
            "error should mention discord.token: {err}"
        );
    }

    #[test]
    fn factory_create_invalid_app_id_is_an_error() {
        let factory = DiscordGatewayFactory::new();
        let err = expect_err(factory.create(&serde_json::json!({
            "discord.token": "t",
            "discord.app_id": "not-a-number",
            "discord.guild_id": "2",
        })));
        assert!(
            err.to_string().contains("discord.app_id"),
            "error should mention discord.app_id: {err}"
        );
    }

    // ---------- id parsing guards (no panics on 0 / non-numeric) ----------

    #[test]
    fn parse_nonzero_id_rejects_zero_and_non_numeric() {
        assert!(parse_nonzero_id("0", "x").is_err());
        assert!(parse_nonzero_id("abc", "x").is_err());
        assert_eq!(parse_nonzero_id("123", "x").unwrap(), 123);
    }

    #[test]
    fn channel_id_from_str_rejects_zero_and_non_numeric() {
        assert!(channel_id_from_str("0").is_none());
        assert!(channel_id_from_str("abc").is_none());
        assert!(channel_id_from_str("123").is_some());
    }
}
