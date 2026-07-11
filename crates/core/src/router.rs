//! Router: both routing directions as two kinds of methods on one struct.
//!
//! - Outbound: consumes `CoreEvent`s off a broadcast channel and renders them
//!   onto every gateway surface bound to the originating session. Renders
//!   are serialized by processing the broadcast stream on a single task
//!   (deliberate divergence from the original TS implementation, which
//!   serialized per-session via promise chains). Render errors are
//!   swallowed: a failed post to one surface must never take down the loop.
//! - Inbound (Task 4): `on_connect`/`on_start`/`on_reply`/`on_end`/`on_stop`
//!   — the gateway-facing entry points a Discord (or other) connector calls
//!   to drive the `/connect` provisioning flow and route messages to
//!   sessions.
//!
//! `run()` still consumes `self` by value (unchanged from before Task 4), so
//! a single `Router` instance cannot both drive the outbound loop AND serve
//! inbound calls — production wiring (and tests) build two instances that
//! share the same `Arc<ControlPlane>`/`Arc<Store>`, one dedicated to `run()`.

use crate::control::{ControlPlane, ProvisionProjectRequest, ProvisionSettings};
use crate::domain::{AttachmentRef, CoreEvent, PermMode, Project};
use crate::gateway::{Gateway, MessageRef};
use crate::harness::TurnPrompt;
use crate::store::Store;
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::broadcast;

/// Split `s` into <=1900-char slices for gateways with per-message size
/// limits. Empty input yields a single `"(done)"` placeholder so callers
/// always have something to post.
pub fn chunk(s: &str) -> Vec<String> {
    if s.is_empty() {
        return vec!["(done)".to_string()];
    }
    s.chars()
        .collect::<Vec<char>>()
        .chunks(1900)
        .map(|c| c.iter().collect())
        .collect()
}

/// Per-session render state: the coalesced assistant-text buffer, and the
/// cached status `MessageRef` per `"{gateway}:{conversation_id}"` surface key
/// (so a session's status line is posted once, then edited in place).
#[derive(Default)]
struct SessionRenderState {
    buffer: String,
    status_refs: HashMap<String, MessageRef>,
}

/// Options for [`Router::on_connect`]: an optional project name, an
/// optional git URL to clone, provisioning settings, and the actor's role
/// ids (for the permission-mode gate).
#[derive(Debug, Clone, Default)]
pub struct ConnectOpts {
    pub name: Option<String>,
    pub git_url: Option<String>,
    pub settings: ProvisionSettings,
    pub actor_role_ids: Vec<String>,
}

/// Outcome of [`Router::on_connect`]: the gateway workspace id, the
/// (possibly newly provisioned) project, and whether a requested
/// `bypassPermissions` was downgraded because the actor isn't an admin.
#[derive(Debug, Clone)]
pub struct ConnectOutcome {
    pub workspace_id: String,
    pub project: Project,
    pub perm_mode_downgraded: bool,
}

/// Renders `CoreEvent`s onto every gateway surface bound to their session
/// (outbound), and dispatches gateway-triggered actions to the control plane
/// (inbound — Task 4).
pub struct Router {
    cp: Arc<ControlPlane>,
    store: Arc<Store>,
    gateways: HashMap<String, Arc<dyn Gateway>>,
    state: HashMap<String, SessionRenderState>,
}

impl Router {
    /// `store` is derived from `cp.store()` — the same persistence handle
    /// the control plane uses, so bindings/surfaces written via inbound
    /// calls (`on_connect`/`on_start`) are immediately visible to outbound
    /// rendering (`run`), even across two `Router` instances (see module doc).
    pub fn new(cp: Arc<ControlPlane>, gateways: Vec<Arc<dyn Gateway>>) -> Self {
        let store = cp.store().clone();
        let gateways = gateways
            .into_iter()
            .map(|g| (g.id().to_string(), g))
            .collect();
        Router {
            cp,
            store,
            gateways,
            state: HashMap::new(),
        }
    }

    /// Provision (or bind) a project for `gateway_id`'s workspace — the
    /// Discord `/connect` flow.
    pub async fn on_connect(
        &self,
        gateway_id: &str,
        actor: &str,
        opts: ConnectOpts,
    ) -> anyhow::Result<ConnectOutcome> {
        let gw = self
            .gateways
            .get(gateway_id)
            .cloned()
            .ok_or_else(|| anyhow::anyhow!("unknown gateway: {gateway_id}"))?;
        // Deliberate divergence from the original TS implementation (which
        // only fell through to `git_url` when `name` was entirely absent,
        // rejecting a bare empty string immediately): here an empty or
        // whitespace-only `name` is also treated as absent and falls through
        // to `git_url`'s derived basename. Either way, an empty/whitespace
        // result below is rejected BEFORE `create_workspace` is ever called,
        // so no gateway workspace is orphaned trying to bind to a project
        // that will fail to provision.
        let name_display = opts.name.as_deref().filter(|n| !n.trim().is_empty());
        let display = match name_display {
            Some(n) => n.to_string(),
            None => opts
                .git_url
                .as_deref()
                .map(|url| {
                    let b = crate::control::basename_of(url);
                    b.strip_suffix(".git").map(str::to_string).unwrap_or(b)
                })
                .unwrap_or_default(),
        };
        if display.trim().is_empty() {
            anyhow::bail!("connect requires name or gitUrl");
        }
        let workspace_id = gw.create_workspace(&display).await?;
        let requested_bypass = opts.settings.perm_mode == Some(PermMode::BypassPermissions);
        let project = self
            .cp
            .provision_project(ProvisionProjectRequest {
                gateway: gateway_id.to_string(),
                workspace_id: workspace_id.clone(),
                actor: actor.to_string(),
                actor_role_ids: opts.actor_role_ids,
                // Forward the SAME filtered name used to compute `display`
                // above, not the raw `opts.name` — otherwise `display`
                // (and thus `create_workspace`) can take the `gitUrl`
                // branch while `provision_project` still sees
                // `Some("")`/`Some("  ")` and takes the name branch,
                // failing `validate_project_name` against an empty string
                // and orphaning the just-created gateway workspace.
                name: name_display.map(str::to_string),
                git_url: opts.git_url,
                settings: opts.settings,
            })
            .await?;
        let perm_mode_downgraded =
            requested_bypass && project.perm_mode != PermMode::BypassPermissions;
        Ok(ConnectOutcome {
            workspace_id,
            project,
            perm_mode_downgraded,
        })
    }

    /// Route an inbound "start" trigger for a connected gateway workspace.
    /// Silently no-ops if the workspace isn't bound to a project (nothing to
    /// route to — the user hasn't `/connect`ed yet).
    ///
    /// Deliberate divergence from the original TS implementation (which
    /// bound the gateway surface atomically inside session start):
    /// `start_session` doesn't take a surface, so this calls
    /// `store.add_surface` right AFTER `start_session` returns instead. A
    /// `status`/`text` event racing ahead of that `add_surface` call would
    /// find no bound surface yet and be silently dropped for this session —
    /// acceptable because `start_session` only returns once the harness
    /// session itself is live (a single async gap), and the final
    /// `Result`/`Error` event (and anything after) is guaranteed to reach
    /// the surface once bound.
    pub async fn on_start(
        &self,
        gateway_id: &str,
        workspace_id: &str,
        actor: &str,
        prompt: &str,
        attachments: &[AttachmentRef],
    ) -> anyhow::Result<()> {
        let Some(project) = self
            .store
            .resolve_project_by_workspace(gateway_id, workspace_id)
            .await?
        else {
            return Ok(());
        };
        let gw = self
            .gateways
            .get(gateway_id)
            .cloned()
            .ok_or_else(|| anyhow::anyhow!("unknown gateway: {gateway_id}"))?;
        let title: String = prompt.chars().take(80).collect();
        let title = if title.is_empty() {
            "session".to_string()
        } else {
            title
        };
        let conversation_id = gw.create_conversation(workspace_id, &title).await?;
        let session = self
            .cp
            .start_session(&project.project_id, prompt, actor, attachments)
            .await?;
        self.store
            .add_surface(gateway_id, &conversation_id, &session.session_pk)
            .await?;
        Ok(())
    }

    /// Continue the session bound to `conversation_id`. Silently no-ops if
    /// no session is bound (e.g. the conversation was never started, or has
    /// already ended).
    pub async fn on_reply(
        &self,
        gateway_id: &str,
        conversation_id: &str,
        _actor: &str,
        prompt: &str,
        attachments: &[AttachmentRef],
    ) -> anyhow::Result<()> {
        let Some(session) = self
            .store
            .resolve_by_conversation(gateway_id, conversation_id)
            .await?
        else {
            return Ok(());
        };
        self.cp
            .continue_session(&session.session_pk, prompt, attachments)
            .await
    }

    /// Route an inbound 1:1 DM (Discord DM today; gateway-agnostic in
    /// principle): no `/connect` binding required. Continues the
    /// project-less `chat` session already bound to `conversation_id`, or
    /// starts a new one via `start_chat_session` and binds it with
    /// `add_surface` — mirroring `on_start`'s post-hoc surface bind (see its
    /// doc) but for a chat session, which needs no project/workspace at
    /// all.
    pub async fn on_dm(
        &self,
        gateway_id: &str,
        conversation_id: &str,
        user_id: &str,
        text: &str,
    ) -> anyhow::Result<()> {
        if let Some(session) = self
            .store
            .resolve_by_conversation(gateway_id, conversation_id)
            .await?
        {
            return self
                .cp
                .continue_session(&session.session_pk, text, &[])
                .await;
        }
        let session = self
            .cp
            .start_chat_session(
                TurnPrompt::text(text, text),
                &format!("{gateway_id}:{user_id}"),
                &[],
            )
            .await?;
        self.store
            .add_surface(gateway_id, conversation_id, &session.session_pk)
            .await?;
        Ok(())
    }

    /// End the session bound to `conversation_id`; no-op if none is bound.
    pub async fn on_end(&self, gateway_id: &str, conversation_id: &str) -> anyhow::Result<()> {
        if let Some(session) = self
            .store
            .resolve_by_conversation(gateway_id, conversation_id)
            .await?
        {
            self.cp.end_session(&session.session_pk).await?;
        }
        Ok(())
    }

    /// Stop the session bound to `conversation_id`; no-op if none is bound.
    pub async fn on_stop(&self, gateway_id: &str, conversation_id: &str) -> anyhow::Result<()> {
        if let Some(session) = self
            .store
            .resolve_by_conversation(gateway_id, conversation_id)
            .await?
        {
            self.cp.stop_session(&session.session_pk).await?;
        }
        Ok(())
    }

    /// Consume the broadcast until it closes (all senders dropped). Intended
    /// to be spawned with `tokio::spawn`. Single-task serialization: render
    /// errors are swallowed so one failed post can never stop the loop.
    pub async fn run(mut self, mut rx: broadcast::Receiver<CoreEvent>) {
        loop {
            match rx.recv().await {
                Ok(event) => self.handle_event(event).await,
                Err(broadcast::error::RecvError::Lagged(n)) => {
                    eprintln!("[router] lagged: dropped {n} events");
                    continue;
                }
                Err(broadcast::error::RecvError::Closed) => break,
            }
        }
    }

    async fn handle_event(&mut self, event: CoreEvent) {
        match event {
            CoreEvent::Message {
                session_pk,
                block_type,
                payload,
                ..
            } => match block_type.as_str() {
                "text" => {
                    if let Some(text) = payload.get("text").and_then(|v| v.as_str()) {
                        self.state
                            .entry(session_pk)
                            .or_default()
                            .buffer
                            .push_str(text);
                    }
                }
                "status" => {
                    if let Some(text) = payload.get("summary").and_then(|v| v.as_str()) {
                        let text = text.to_string();
                        self.render_status(&session_pk, &text).await;
                    }
                }
                _ => {}
            },
            CoreEvent::Result { session_pk } => self.render_result(&session_pk).await,
            CoreEvent::Error {
                session_pk,
                message,
            } => self.render_error(&session_pk, &message).await,
            CoreEvent::Notice { session_pk, text } => self.render_notice(&session_pk, &text).await,
            CoreEvent::SessionEnded { session_pk } => {
                self.state.remove(&session_pk);
            }
            CoreEvent::SessionCreated { .. }
            | CoreEvent::ApprovalRequested { .. }
            | CoreEvent::JobRunChanged { .. }
            | CoreEvent::OrchTaskChanged { .. }
            // Context telemetry has no Discord rendering (yet) — the
            // compaction notice arrives as a persisted Message row instead.
            | CoreEvent::ContextUsage { .. }
            | CoreEvent::ContextCompacted { .. }
            // OAuth authorize-URL events are a Cockpit-only browser-open
            // signal — Discord has no matching surface.
            | CoreEvent::OauthAuthorizeUrl { .. }
            | CoreEvent::PluginOauthAuthorizeUrl { .. }
            // Per-session cost telemetry is a Cockpit-only surface too.
            | CoreEvent::SessionCost { .. } => {}
        }
    }

    async fn render_status(&mut self, session_pk: &str, text: &str) {
        let surfaces = self.store.surfaces(session_pk).await.unwrap_or_default();
        for surface in surfaces {
            let Some(gw) = self.gateways.get(&surface.gateway).cloned() else {
                continue;
            };
            let key = format!("{}:{}", surface.gateway, surface.conversation_id);
            let existing = self
                .state
                .get(session_pk)
                .and_then(|st| st.status_refs.get(&key).cloned());
            if let Some(msg_ref) = existing {
                let _ = gw.edit_status(&msg_ref, text).await;
            } else if let Ok(msg_ref) = gw.post_status(&surface, text).await {
                self.state
                    .entry(session_pk.to_string())
                    .or_default()
                    .status_refs
                    .insert(key, msg_ref);
            }
        }
    }

    async fn render_result(&mut self, session_pk: &str) {
        let buffer = self
            .state
            .get(session_pk)
            .map(|st| st.buffer.clone())
            .unwrap_or_default();
        let chunks = chunk(&buffer);
        let surfaces = self.store.surfaces(session_pk).await.unwrap_or_default();
        for surface in surfaces {
            if let Some(gw) = self.gateways.get(&surface.gateway) {
                let _ = gw.post_result(&surface, &chunks).await;
            }
        }
        self.state.remove(session_pk);
    }

    async fn render_error(&mut self, session_pk: &str, message: &str) {
        let surfaces = self.store.surfaces(session_pk).await.unwrap_or_default();
        for surface in surfaces {
            if let Some(gw) = self.gateways.get(&surface.gateway) {
                let _ = gw.post_error(&surface, message).await;
            }
        }
        self.state.remove(session_pk);
    }

    /// Post the notice text as a result chunk to every surface.
    async fn render_notice(&mut self, session_pk: &str, text: &str) {
        let surfaces = self.store.surfaces(session_pk).await.unwrap_or_default();
        for surface in surfaces {
            if let Some(gw) = self.gateways.get(&surface.gateway) {
                let _ = gw.post_result(&surface, &[text.to_string()]).await;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::{ApprovalDecision, ApprovalRequest, Surface};
    use async_trait::async_trait;
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::sync::Mutex;

    #[test]
    fn chunk_empty_input_yields_done_placeholder() {
        assert_eq!(chunk(""), vec!["(done)".to_string()]);
    }

    #[test]
    fn chunk_boundary_at_1900_chars_is_a_single_chunk() {
        let s = "a".repeat(1900);
        let chunks = chunk(&s);
        assert_eq!(chunks.len(), 1);
        assert_eq!(chunks[0].chars().count(), 1900);
    }

    #[test]
    fn chunk_boundary_at_1901_chars_splits_into_two() {
        let s = "a".repeat(1901);
        let chunks = chunk(&s);
        assert_eq!(chunks.len(), 2);
        assert_eq!(chunks[0].chars().count(), 1900);
        assert_eq!(chunks[1].chars().count(), 1);
    }

    #[test]
    fn chunk_boundary_at_3800_chars_splits_into_two_equal_chunks() {
        let s = "a".repeat(3800);
        let chunks = chunk(&s);
        assert_eq!(chunks.len(), 2);
        assert_eq!(chunks[0].chars().count(), 1900);
        assert_eq!(chunks[1].chars().count(), 1900);
    }

    /// Records every call in order; `post_status` hands back incrementing
    /// message ids so `edit_status` targeting can be asserted.
    struct FakeGateway {
        gid: String,
        calls: Mutex<Vec<String>>,
        n: AtomicU64,
        chunks_log: Mutex<Vec<Vec<String>>>,
    }

    impl FakeGateway {
        fn new(gid: &str) -> Self {
            FakeGateway {
                gid: gid.to_string(),
                calls: Mutex::new(Vec::new()),
                n: AtomicU64::new(0),
                chunks_log: Mutex::new(Vec::new()),
            }
        }

        fn calls(&self) -> Vec<String> {
            self.calls.lock().unwrap().clone()
        }
    }

    #[async_trait]
    impl Gateway for FakeGateway {
        fn id(&self) -> &str {
            &self.gid
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
        async fn post_status(&self, surface: &Surface, text: &str) -> anyhow::Result<MessageRef> {
            let n = self.n.fetch_add(1, Ordering::SeqCst);
            self.calls
                .lock()
                .unwrap()
                .push(format!("post_status:{}:{}", surface.conversation_id, text));
            Ok(MessageRef {
                surface: surface.clone(),
                message_id: format!("m{n}"),
            })
        }
        async fn edit_status(&self, msg: &MessageRef, text: &str) -> anyhow::Result<()> {
            self.calls
                .lock()
                .unwrap()
                .push(format!("edit_status:{}:{}", msg.message_id, text));
            Ok(())
        }
        async fn post_result(&self, surface: &Surface, chunks: &[String]) -> anyhow::Result<()> {
            self.calls.lock().unwrap().push(format!(
                "post_result:{}:{}",
                surface.conversation_id,
                chunks.len()
            ));
            self.chunks_log.lock().unwrap().push(chunks.to_vec());
            Ok(())
        }
        async fn post_error(&self, surface: &Surface, message: &str) -> anyhow::Result<()> {
            self.calls.lock().unwrap().push(format!(
                "post_error:{}:{}",
                surface.conversation_id, message
            ));
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

    /// A `ControlPlane` (no harness registered — outbound-only tests never
    /// start a session) plus a clone of its internal `Store` for direct
    /// seeding (`add_surface`) and assertions. The underlying sqlite temp
    /// file is intentionally NOT kept alive past this call — same pattern as
    /// `control.rs`'s helpers: the pool's already-open fd keeps working
    /// after the path is unlinked.
    async fn test_control_plane() -> (Arc<ControlPlane>, Arc<Store>) {
        let tmp = tempfile::NamedTempFile::new().unwrap();
        let store = Store::open(tmp.path()).await.unwrap();
        let cp = ControlPlane::new(store, crate::plugins::Registries::new()).await;
        let store_ref = cp.store().clone();
        (cp, store_ref)
    }

    fn text_event(session_pk: &str, seq: i64, text: &str) -> CoreEvent {
        CoreEvent::Message {
            session_pk: session_pk.into(),
            seq,
            role: "assistant".into(),
            block_type: "text".into(),
            payload: serde_json::json!({ "text": text }),
            tool_call_id: None,
            status: None,
            tool_kind: None,
        }
    }

    fn status_event(session_pk: &str, text: &str) -> CoreEvent {
        CoreEvent::Message {
            session_pk: session_pk.into(),
            seq: 1,
            role: "system".into(),
            block_type: "status".into(),
            payload: serde_json::json!({ "summary": text }),
            tool_call_id: None,
            status: None,
            tool_kind: None,
        }
    }

    #[tokio::test]
    async fn text_chunks_buffer_and_flush_as_one_post_result_on_result() {
        let (cp, store) = test_control_plane().await;
        store.add_surface("fake", "c1", "s1").await.unwrap();
        let gw = Arc::new(FakeGateway::new("fake"));
        let (tx, rx) = broadcast::channel(16);
        let router = Router::new(Arc::clone(&cp), vec![gw.clone() as Arc<dyn Gateway>]);
        let handle = tokio::spawn(router.run(rx));

        tx.send(text_event("s1", 1, &"a".repeat(1000))).unwrap();
        tx.send(text_event("s1", 2, &"b".repeat(1200))).unwrap();
        tx.send(CoreEvent::Result {
            session_pk: "s1".into(),
        })
        .unwrap();
        drop(tx);
        handle.await.unwrap();

        let calls = gw.calls();
        assert_eq!(calls, vec!["post_result:c1:2".to_string()]);
    }

    #[tokio::test]
    async fn status_posts_once_then_edits() {
        let (cp, store) = test_control_plane().await;
        store.add_surface("fake", "c1", "s1").await.unwrap();
        let gw = Arc::new(FakeGateway::new("fake"));
        let (tx, rx) = broadcast::channel(16);
        let router = Router::new(Arc::clone(&cp), vec![gw.clone() as Arc<dyn Gateway>]);
        let handle = tokio::spawn(router.run(rx));

        tx.send(status_event("s1", "working")).unwrap();
        tx.send(status_event("s1", "still working")).unwrap();
        drop(tx);
        handle.await.unwrap();

        assert_eq!(
            gw.calls(),
            vec![
                "post_status:c1:working".to_string(),
                "edit_status:m0:still working".to_string(),
            ]
        );
    }

    #[tokio::test]
    async fn status_posts_once_per_surface_across_gateways_with_independent_refs() {
        // Two surfaces on TWO DIFFERENT gateways bound to the same session:
        // each surface must get its own post-then-edit ref, independent of
        // the other surface's.
        let (cp, store) = test_control_plane().await;
        store.add_surface("fake1", "c1", "s1").await.unwrap();
        store.add_surface("fake2", "c2", "s1").await.unwrap();
        let gw1 = Arc::new(FakeGateway::new("fake1"));
        let gw2 = Arc::new(FakeGateway::new("fake2"));
        let (tx, rx) = broadcast::channel(16);
        let router = Router::new(
            Arc::clone(&cp),
            vec![
                gw1.clone() as Arc<dyn Gateway>,
                gw2.clone() as Arc<dyn Gateway>,
            ],
        );
        let handle = tokio::spawn(router.run(rx));

        tx.send(status_event("s1", "working")).unwrap();
        tx.send(status_event("s1", "still working")).unwrap();
        drop(tx);
        handle.await.unwrap();

        assert_eq!(
            gw1.calls(),
            vec![
                "post_status:c1:working".to_string(),
                "edit_status:m0:still working".to_string(),
            ]
        );
        assert_eq!(
            gw2.calls(),
            vec![
                "post_status:c2:working".to_string(),
                "edit_status:m0:still working".to_string(),
            ]
        );
    }

    #[tokio::test]
    async fn error_posts_then_a_later_result_renders_done_for_dropped_state() {
        let (cp, store) = test_control_plane().await;
        store.add_surface("fake", "c1", "s1").await.unwrap();
        let gw = Arc::new(FakeGateway::new("fake"));
        let (tx, rx) = broadcast::channel(16);
        let router = Router::new(Arc::clone(&cp), vec![gw.clone() as Arc<dyn Gateway>]);
        let handle = tokio::spawn(router.run(rx));

        tx.send(CoreEvent::Error {
            session_pk: "s1".into(),
            message: "boom".into(),
        })
        .unwrap();
        tx.send(CoreEvent::Result {
            session_pk: "s1".into(),
        })
        .unwrap();
        drop(tx);
        handle.await.unwrap();

        assert_eq!(
            gw.calls(),
            vec![
                "post_error:c1:boom".to_string(),
                "post_result:c1:1".to_string(), // chunk("") => ["(done)"]
            ]
        );
    }

    #[tokio::test]
    async fn notice_posts_the_text_to_every_surface_without_touching_the_buffer() {
        let (cp, store) = test_control_plane().await;
        store.add_surface("fake", "c1", "s1").await.unwrap();
        store.add_surface("fake", "c2", "s1").await.unwrap();
        let gw = Arc::new(FakeGateway::new("fake"));
        let (tx, rx) = broadcast::channel(16);
        let router = Router::new(Arc::clone(&cp), vec![gw.clone() as Arc<dyn Gateway>]);
        let handle = tokio::spawn(router.run(rx));

        tx.send(text_event("s1", 1, "buffered")).unwrap();
        tx.send(CoreEvent::Notice {
            session_pk: "s1".into(),
            text: "⬆️ ryuzi 0.3.0 is available - hint".into(),
        })
        .unwrap();
        tx.send(CoreEvent::Result {
            session_pk: "s1".into(),
        })
        .unwrap();
        drop(tx);
        handle.await.unwrap();

        let calls = gw.calls();
        assert_eq!(
            calls.len(),
            4,
            "notice → 2 surfaces, then result flush → 2 surfaces: {calls:?}"
        );
        let mut notice_posts = calls[..2].to_vec();
        notice_posts.sort();
        assert_eq!(notice_posts, vec!["post_result:c1:1", "post_result:c2:1"]);

        let chunks = gw.chunks_log.lock().unwrap().clone();
        assert_eq!(
            chunks[0],
            vec!["⬆️ ryuzi 0.3.0 is available - hint".to_string()]
        );
        assert_eq!(
            chunks[1],
            vec!["⬆️ ryuzi 0.3.0 is available - hint".to_string()]
        );
        assert_eq!(
            chunks[2],
            vec!["buffered".to_string()],
            "notice must not consume the text buffer"
        );
    }

    #[tokio::test]
    async fn events_for_sessions_with_no_surfaces_are_ignored() {
        let (cp, _store) = test_control_plane().await;
        // Deliberately no add_surface for "s1".
        let gw = Arc::new(FakeGateway::new("fake"));
        let (tx, rx) = broadcast::channel(16);
        let router = Router::new(Arc::clone(&cp), vec![gw.clone() as Arc<dyn Gateway>]);
        let handle = tokio::spawn(router.run(rx));

        tx.send(text_event("s1", 1, "hello")).unwrap();
        tx.send(CoreEvent::Result {
            session_pk: "s1".into(),
        })
        .unwrap();
        drop(tx);
        handle.await.unwrap();

        assert!(gw.calls().is_empty());
    }

    #[tokio::test]
    async fn unknown_gateway_id_is_skipped() {
        let (cp, store) = test_control_plane().await;
        // Surface bound to "other", but the router only knows "fake".
        store.add_surface("other", "c1", "s1").await.unwrap();
        let gw = Arc::new(FakeGateway::new("fake"));
        let (tx, rx) = broadcast::channel(16);
        let router = Router::new(Arc::clone(&cp), vec![gw.clone() as Arc<dyn Gateway>]);
        let handle = tokio::spawn(router.run(rx));

        tx.send(text_event("s1", 1, "hello")).unwrap();
        tx.send(CoreEvent::Result {
            session_pk: "s1".into(),
        })
        .unwrap();
        drop(tx);
        handle.await.unwrap();

        assert!(gw.calls().is_empty());
    }

    // ---------- Task 4: inbound Router (on_connect/on_start/on_reply/on_end/on_stop) ----------

    use serial_test::serial;

    /// Redirect dirs::data_dir() into a tempdir for the duration of a test so
    /// worktree creation (triggered by `on_start` -> `start_session`) never
    /// touches the real `~/.local/share`, and drop a `.gitconfig` under the
    /// redirected `HOME` so `on_connect`'s real `git commit` (via
    /// `provision_project`'s name-flow) has a resolvable identity. Process-
    /// global env — every test using it must be `#[serial]`.
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

    /// A harness session that completes its turn immediately — for tests
    /// that don't care about exact interleaving with the outbound loop.
    struct OneShotSession;
    #[async_trait]
    impl crate::harness::HarnessSession for OneShotSession {
        async fn send_prompt(&self, _prompt: crate::harness::TurnPrompt) -> anyhow::Result<()> {
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
    impl crate::harness::Harness for OneShotHarness {
        async fn start_session(
            &self,
            _ctx: crate::harness::SessionCtx,
        ) -> anyhow::Result<Box<dyn crate::harness::HarnessSession>> {
            Ok(Box::new(OneShotSession))
        }
    }
    struct OneShotHarnessFactory;
    impl crate::harness::HarnessFactory for OneShotHarnessFactory {
        fn create(&self) -> anyhow::Result<Arc<dyn crate::harness::Harness>> {
            Ok(Arc::new(OneShotHarness))
        }
    }

    /// A harness session whose turn blocks until the test calls
    /// `proceed.notify_one()` — used to deterministically prove that
    /// `on_start`'s `add_surface` call (which happens BEFORE the turn is
    /// released here) has already run by the time the session's `Result`
    /// event reaches the outbound loop, instead of relying on the
    /// documented (and otherwise racy) add_surface-vs-Result timing.
    struct GatedSession {
        proceed: Arc<tokio::sync::Notify>,
    }
    #[async_trait]
    impl crate::harness::HarnessSession for GatedSession {
        async fn send_prompt(&self, _prompt: crate::harness::TurnPrompt) -> anyhow::Result<()> {
            self.proceed.notified().await;
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
    struct GatedHarness {
        proceed: Arc<tokio::sync::Notify>,
    }
    #[async_trait]
    impl crate::harness::Harness for GatedHarness {
        async fn start_session(
            &self,
            _ctx: crate::harness::SessionCtx,
        ) -> anyhow::Result<Box<dyn crate::harness::HarnessSession>> {
            Ok(Box::new(GatedSession {
                proceed: self.proceed.clone(),
            }))
        }
    }
    struct GatedHarnessFactory {
        proceed: Arc<tokio::sync::Notify>,
    }
    impl crate::harness::HarnessFactory for GatedHarnessFactory {
        fn create(&self) -> anyhow::Result<Arc<dyn crate::harness::Harness>> {
            Ok(Arc::new(GatedHarness {
                proceed: self.proceed.clone(),
            }))
        }
    }

    /// A `ControlPlane` wired with `harness` as the single native slot
    /// and `workdir_root` pointed at `root` (needed by `on_connect` ->
    /// `provision_project`'s name-flow). Returns the sqlite temp-file guard
    /// the caller must keep alive.
    async fn wired_control_plane_with_harness(
        root: &std::path::Path,
        harness: Arc<dyn crate::harness::HarnessFactory>,
    ) -> (Arc<ControlPlane>, Arc<Store>, tempfile::NamedTempFile) {
        let db_guard = tempfile::NamedTempFile::new().unwrap();
        let store = Store::open(db_guard.path()).await.unwrap();
        let mut regs = crate::plugins::Registries::new();
        regs.harness = harness;
        let cp = ControlPlane::new(store, regs).await;
        let store_ref = cp.store().clone();
        crate::settings::SettingsStore::new(store_ref.clone())
            .set("workdir_root", root.to_str().unwrap())
            .await
            .unwrap();
        (cp, store_ref, db_guard)
    }

    /// Like `wired_control_plane_with_harness`, defaulted to the
    /// non-blocking `OneShotHarnessFactory`.
    async fn wired_control_plane(
        root: &std::path::Path,
    ) -> (Arc<ControlPlane>, Arc<Store>, tempfile::NamedTempFile) {
        wired_control_plane_with_harness(root, Arc::new(OneShotHarnessFactory)).await
    }

    /// Poll `store.list_sessions(None)` until at least `n` rows exist (or panic).
    async fn wait_for_sessions(store: &Store, n: usize) -> Vec<crate::domain::Session> {
        for _ in 0..300 {
            let s = store.list_sessions(None).await.unwrap();
            if s.len() >= n {
                return s;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
        panic!("timed out waiting for {n} session(s)");
    }

    /// Poll a session's status until it matches `status` (or panic).
    async fn wait_for_status(store: &Store, pk: &str, status: crate::domain::SessionStatus) {
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

    #[tokio::test]
    #[serial]
    async fn on_connect_creates_workspace_and_binds_project() {
        let _guard = StateDirGuard::new();
        let root = tempfile::tempdir().unwrap();
        let (cp, store, _db_guard) = wired_control_plane(root.path()).await;
        let gw = Arc::new(FakeGateway::new("fake"));
        let router = Router::new(Arc::clone(&cp), vec![gw.clone() as Arc<dyn Gateway>]);

        let outcome = router
            .on_connect(
                "fake",
                "u1",
                ConnectOpts {
                    name: Some("foo".into()),
                    ..Default::default()
                },
            )
            .await
            .unwrap();

        assert!(gw.calls().contains(&"create_workspace:foo".to_string()));
        let bound = store
            .resolve_project_by_workspace("fake", &outcome.workspace_id)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(bound.project_id, outcome.project.project_id);
    }

    #[tokio::test]
    #[serial]
    async fn on_connect_unknown_gateway_errors() {
        let (cp, _store) = test_control_plane().await;
        let router = Router::new(Arc::clone(&cp), vec![]);
        let err = router
            .on_connect("nope", "u1", ConnectOpts::default())
            .await
            .unwrap_err();
        assert_eq!(err.to_string(), "unknown gateway: nope");
    }

    #[tokio::test]
    #[serial]
    async fn on_connect_requires_name_or_git_url() {
        let (cp, _store) = test_control_plane().await;
        let gw = Arc::new(FakeGateway::new("fake"));
        let router = Router::new(Arc::clone(&cp), vec![gw.clone() as Arc<dyn Gateway>]);
        let err = router
            .on_connect("fake", "u1", ConnectOpts::default())
            .await
            .unwrap_err();
        assert_eq!(err.to_string(), "connect requires name or gitUrl");
    }

    /// Regression for the parity gap where an empty `name` (`Some("")`)
    /// passed the old `Option::is_none()` check and reached
    /// `gw.create_workspace("")` before failing later inside
    /// `provision_project` — leaving an orphaned gateway workspace bound to
    /// nothing. `on_connect` must now bail with the same "requires name or
    /// gitUrl" error BEFORE ever calling `create_workspace`.
    #[tokio::test]
    #[serial]
    async fn on_connect_with_empty_name_and_no_git_url_bails_before_creating_a_workspace() {
        let (cp, _store) = test_control_plane().await;
        let gw = Arc::new(FakeGateway::new("fake"));
        let router = Router::new(Arc::clone(&cp), vec![gw.clone() as Arc<dyn Gateway>]);
        let err = router
            .on_connect(
                "fake",
                "u1",
                ConnectOpts {
                    name: Some("".to_string()),
                    ..Default::default()
                },
            )
            .await
            .unwrap_err();
        assert_eq!(err.to_string(), "connect requires name or gitUrl");
        assert!(
            gw.calls().is_empty(),
            "create_workspace must not be called when the display name is empty; calls: {:?}",
            gw.calls()
        );
    }

    /// Regression for the raw-`opts.name`-forwarding bug: `display` (and
    /// thus `create_workspace`) already treats `Some("")` as absent and
    /// falls through to the `gitUrl`-derived basename, so
    /// `provision_project` must be handed that SAME filtered name — not
    /// the raw `Some("")` — otherwise it takes the name branch and fails
    /// `validate_project_name("")` instead of trying the `gitUrl` clone,
    /// orphaning the just-created gateway workspace and surfacing the
    /// wrong error.
    #[tokio::test]
    #[serial]
    async fn on_connect_with_empty_name_and_git_url_takes_the_git_url_branch() {
        let _guard = StateDirGuard::new();
        let root = tempfile::tempdir().unwrap();
        let (cp, _store, _db_guard) = wired_control_plane(root.path()).await;
        let gw = Arc::new(FakeGateway::new("fake"));
        let router = Router::new(Arc::clone(&cp), vec![gw.clone() as Arc<dyn Gateway>]);

        let err = router
            .on_connect(
                "fake",
                "u1",
                ConnectOpts {
                    name: Some("".to_string()),
                    git_url: Some("/no/such/upstream/myrepo.git".to_string()),
                    ..Default::default()
                },
            )
            .await
            .unwrap_err();

        // `display` fell through to gitUrl's derived basename ("myrepo"),
        // so the gateway workspace was created under that name...
        assert!(
            gw.calls().contains(&"create_workspace:myrepo".to_string()),
            "expected create_workspace:myrepo, got: {:?}",
            gw.calls()
        );
        // ...and `provision_project` must have taken the SAME (gitUrl)
        // branch: a clone-failure error, never the name-validation error
        // that `Some("")` would trigger if forwarded raw.
        let msg = err.to_string();
        assert!(
            !msg.contains("invalid project name"),
            "provision_project must not take the name branch for an empty name; got: {msg}"
        );
        assert!(
            msg.contains("git"),
            "expected a git-clone failure message, got: {msg}"
        );
    }

    #[tokio::test]
    #[serial]
    async fn on_connect_downgrades_bypass_permissions_for_non_admin() {
        let _guard = StateDirGuard::new();
        let root = tempfile::tempdir().unwrap();
        let (cp, store, _db_guard) = wired_control_plane(root.path()).await;
        crate::settings::SettingsStore::new(Arc::clone(&store))
            .set("admin_role_ids", "admin-role")
            .await
            .unwrap();
        let gw = Arc::new(FakeGateway::new("fake"));
        let router = Router::new(Arc::clone(&cp), vec![gw.clone() as Arc<dyn Gateway>]);

        let outcome = router
            .on_connect(
                "fake",
                "u1",
                ConnectOpts {
                    name: Some("gated".into()),
                    settings: ProvisionSettings {
                        perm_mode: Some(PermMode::BypassPermissions),
                        ..Default::default()
                    },
                    actor_role_ids: vec![], // not an admin
                    ..Default::default()
                },
            )
            .await
            .unwrap();

        assert!(outcome.perm_mode_downgraded);
        assert_eq!(outcome.project.perm_mode, PermMode::Default);
    }

    #[tokio::test]
    #[serial]
    async fn on_start_opens_a_conversation_and_post_result_arrives_via_the_outbound_loop() {
        let _guard = StateDirGuard::new();
        let root = tempfile::tempdir().unwrap();
        let proceed = Arc::new(tokio::sync::Notify::new());
        let (cp, store, _db_guard) = wired_control_plane_with_harness(
            root.path(),
            Arc::new(GatedHarnessFactory {
                proceed: proceed.clone(),
            }),
        )
        .await;
        let gw = Arc::new(FakeGateway::new("fake"));
        let gateways: Vec<Arc<dyn Gateway>> = vec![gw.clone() as Arc<dyn Gateway>];
        // Two Router instances sharing the same `cp`/`store`: one drives the
        // outbound loop (consumes `self` in `run`), the other issues the
        // inbound calls — see the module doc for why this split exists.
        let router_in = Router::new(Arc::clone(&cp), gateways.clone());
        let router_out = Router::new(Arc::clone(&cp), gateways.clone());
        let rx = cp.subscribe();
        let handle = tokio::spawn(router_out.run(rx));

        let outcome = router_in
            .on_connect(
                "fake",
                "u1",
                ConnectOpts {
                    name: Some("bar".into()),
                    ..Default::default()
                },
            )
            .await
            .unwrap();
        router_in
            .on_start("fake", &outcome.workspace_id, "u1", "do the thing", &[])
            .await
            .unwrap();
        // `on_start` only returns after `add_surface` has run — releasing the
        // gated turn here guarantees the surface is bound before `Result` fires.
        proceed.notify_one();

        for _ in 0..300 {
            if gw.calls().iter().any(|c| c.starts_with("post_result:")) {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }

        assert!(gw
            .calls()
            .iter()
            .any(|c| c.starts_with("create_conversation:")));
        assert!(
            gw.calls().iter().any(|c| c.starts_with("post_result:")),
            "post_result must arrive via the outbound loop; calls: {:?}",
            gw.calls()
        );
        assert_eq!(store.list_sessions(None).await.unwrap().len(), 1);

        handle.abort();
    }

    #[tokio::test]
    #[serial]
    async fn on_start_in_an_unconnected_workspace_is_ignored() {
        let _guard = StateDirGuard::new();
        let root = tempfile::tempdir().unwrap();
        let (cp, store, _db_guard) = wired_control_plane(root.path()).await;
        let gw = Arc::new(FakeGateway::new("fake"));
        let router = Router::new(Arc::clone(&cp), vec![gw.clone() as Arc<dyn Gateway>]);

        router
            .on_start("fake", "no-such-ws", "u1", "hello", &[])
            .await
            .unwrap();

        assert!(store.list_sessions(None).await.unwrap().is_empty());
        assert!(!gw
            .calls()
            .iter()
            .any(|c| c.starts_with("create_conversation:")));
    }

    #[tokio::test]
    #[serial]
    async fn on_reply_for_an_unknown_conversation_is_ignored() {
        let _guard = StateDirGuard::new();
        let root = tempfile::tempdir().unwrap();
        let (cp, store, _db_guard) = wired_control_plane(root.path()).await;
        let router = Router::new(Arc::clone(&cp), vec![]);

        router
            .on_reply("fake", "no-such-conv", "u1", "hello", &[])
            .await
            .unwrap();

        assert!(store.list_sessions(None).await.unwrap().is_empty());
    }

    #[tokio::test]
    #[serial]
    async fn on_reply_continues_the_session_for_that_conversation() {
        let _guard = StateDirGuard::new();
        let root = tempfile::tempdir().unwrap();
        let (cp, store, _db_guard) = wired_control_plane(root.path()).await;
        let gw = Arc::new(FakeGateway::new("fake"));
        let router = Router::new(Arc::clone(&cp), vec![gw.clone() as Arc<dyn Gateway>]);

        let outcome = router
            .on_connect(
                "fake",
                "u1",
                ConnectOpts {
                    name: Some("baz".into()),
                    ..Default::default()
                },
            )
            .await
            .unwrap();
        router
            .on_start("fake", &outcome.workspace_id, "u1", "first", &[])
            .await
            .unwrap();

        let sessions = wait_for_sessions(&store, 1).await;
        let session_pk = sessions[0].session_pk.clone();
        wait_for_status(&store, &session_pk, crate::domain::SessionStatus::Idle).await;

        let conv = store.surfaces(&session_pk).await.unwrap()[0]
            .conversation_id
            .clone();
        router
            .on_reply("fake", &conv, "u1", "second", &[])
            .await
            .unwrap();

        wait_for_status(&store, &session_pk, crate::domain::SessionStatus::Idle).await;
        let got = store.get_session(&session_pk).await.unwrap().unwrap();
        assert_eq!(got.status, crate::domain::SessionStatus::Idle); // ran and settled
    }

    #[tokio::test]
    #[serial]
    async fn on_end_ends_the_session_bound_to_the_conversation() {
        let _guard = StateDirGuard::new();
        let root = tempfile::tempdir().unwrap();
        let (cp, store, _db_guard) = wired_control_plane(root.path()).await;
        let gw = Arc::new(FakeGateway::new("fake"));
        let router = Router::new(Arc::clone(&cp), vec![gw.clone() as Arc<dyn Gateway>]);

        let outcome = router
            .on_connect(
                "fake",
                "u1",
                ConnectOpts {
                    name: Some("end-me".into()),
                    ..Default::default()
                },
            )
            .await
            .unwrap();
        router
            .on_start("fake", &outcome.workspace_id, "u1", "go", &[])
            .await
            .unwrap();
        let sessions = wait_for_sessions(&store, 1).await;
        let session_pk = sessions[0].session_pk.clone();
        let conv = store.surfaces(&session_pk).await.unwrap()[0]
            .conversation_id
            .clone();

        router.on_end("fake", &conv).await.unwrap();

        let got = store.get_session(&session_pk).await.unwrap().unwrap();
        assert_eq!(got.status, crate::domain::SessionStatus::Ended);
    }

    #[tokio::test]
    #[serial]
    async fn on_stop_stops_the_session_bound_to_the_conversation() {
        let _guard = StateDirGuard::new();
        let root = tempfile::tempdir().unwrap();
        let (cp, store, _db_guard) = wired_control_plane(root.path()).await;
        let gw = Arc::new(FakeGateway::new("fake"));
        let router = Router::new(Arc::clone(&cp), vec![gw.clone() as Arc<dyn Gateway>]);

        let outcome = router
            .on_connect(
                "fake",
                "u1",
                ConnectOpts {
                    name: Some("stop-me".into()),
                    ..Default::default()
                },
            )
            .await
            .unwrap();
        router
            .on_start("fake", &outcome.workspace_id, "u1", "go", &[])
            .await
            .unwrap();
        let sessions = wait_for_sessions(&store, 1).await;
        let session_pk = sessions[0].session_pk.clone();
        let conv = store.surfaces(&session_pk).await.unwrap()[0]
            .conversation_id
            .clone();

        router.on_stop("fake", &conv).await.unwrap();

        let got = store.get_session(&session_pk).await.unwrap().unwrap();
        assert_eq!(got.status, crate::domain::SessionStatus::Interrupted);
    }

    #[tokio::test]
    #[serial]
    async fn on_end_and_on_stop_for_unknown_conversation_are_silent_no_ops() {
        let (cp, _store) = test_control_plane().await;
        let router = Router::new(Arc::clone(&cp), vec![]);
        router.on_end("fake", "no-such-conv").await.unwrap();
        router.on_stop("fake", "no-such-conv").await.unwrap();
    }

    // ---------- Task A7: on_dm (project-less chat sessions, no /connect) ----------

    /// A DM inbound with no workspace/project binding still starts a
    /// (project-less) `chat` session, bound to the DM conversation via
    /// `add_surface` — proving `on_dm` never consults
    /// `resolve_project_by_workspace` (unlike `on_start`).
    #[tokio::test]
    #[serial]
    async fn discord_dm_starts_a_chat_session() {
        let _guard = StateDirGuard::new();
        let root = tempfile::tempdir().unwrap();
        let (cp, store, _db_guard) = wired_control_plane(root.path()).await;
        let gateways: Vec<Arc<dyn Gateway>> = vec![];
        let router = Router::new(Arc::clone(&cp), gateways);

        router
            .on_dm("discord", "dm-conv-1", "user-9", "hello there")
            .await
            .unwrap();

        let s = store
            .resolve_by_conversation("discord", "dm-conv-1")
            .await
            .unwrap();
        assert!(s.is_some(), "expected a chat session bound to the DM");
        assert_eq!(s.unwrap().kind, crate::domain::SessionKind::Chat);
    }

    /// A second DM in the same conversation continues the already-bound
    /// chat session instead of starting a new one.
    #[tokio::test]
    #[serial]
    async fn discord_dm_second_message_continues_the_same_chat_session() {
        let _guard = StateDirGuard::new();
        let root = tempfile::tempdir().unwrap();
        let (cp, store, _db_guard) = wired_control_plane(root.path()).await;
        let router = Router::new(Arc::clone(&cp), vec![]);

        router
            .on_dm("discord", "dm-conv-1", "user-9", "hello there")
            .await
            .unwrap();
        let sessions = wait_for_sessions(&store, 1).await;
        let session_pk = sessions[0].session_pk.clone();
        wait_for_status(&store, &session_pk, crate::domain::SessionStatus::Idle).await;

        router
            .on_dm("discord", "dm-conv-1", "user-9", "second message")
            .await
            .unwrap();

        // Still exactly one session — the second on_dm continued it.
        assert_eq!(store.list_sessions(None).await.unwrap().len(), 1);
        wait_for_status(&store, &session_pk, crate::domain::SessionStatus::Idle).await;
    }
}
