//! `DashboardState` — pure, terminal-free state machine for the main app
//! view: tab navigation, the sessions list/detail, and inline config editing
//! with provider (gateway/runtime) toggles. Rendering lives in `render.rs`.

use ryuzi_core::settings::ConfigField;

use crate::tui::controller::{AppController, SessionRow};
use crate::tui::Key;

pub const TABS: [&str; 4] = ["Status", "Daemon", "Sessions", "Config"];

/// One row of the Config tab's flattened row list. Headers are never
/// selectable; `config_cursor` walks only the `Field`/`ToggleGateway`/
/// `ToggleRuntime` rows.
pub enum ConfigRow {
    Header(String),
    Field(&'static ConfigField),
    ToggleGateway { id: String, label: String },
    ToggleRuntime { id: String, label: String },
}

pub struct DashboardState {
    pub active: usize,
    pub show_options: bool,
    pub exit: bool,
    pub sessions: Vec<SessionRow>,
    pub sessions_cursor: usize,
    pub detail: Option<usize>,
    /// The full row list (headers included); `config_cursor` indexes the
    /// *selectable* subset, not this vec directly.
    pub config_rows: Vec<ConfigRow>,
    pub config_cursor: usize,
    /// `Some(draft)` while a `Field` row is being edited.
    pub editing: Option<String>,
    pub config_error: Option<String>,
}

impl DashboardState {
    /// Loads the initial sessions list and config rows from `controller`.
    pub async fn new(controller: &AppController) -> Self {
        Self {
            active: 0,
            show_options: false,
            exit: false,
            sessions: controller.sessions().await,
            sessions_cursor: 0,
            detail: None,
            config_rows: build_config_rows(controller).await,
            config_cursor: 0,
            editing: None,
            config_error: None,
        }
    }

    /// Re-pulls sessions and config rows from `controller` (driven by the
    /// dashboard's 1s tick) and clamps cursors that outlived a shrunk list.
    pub async fn refresh(&mut self, controller: &AppController) {
        self.sessions = controller.sessions().await;
        self.rebuild_config_rows(controller).await;

        self.sessions_cursor = clamp_cursor(self.sessions_cursor, self.sessions.len());
        if matches!(self.detail, Some(i) if i >= self.sessions.len()) {
            self.detail = None;
        }
    }

    /// Re-pulls `config_rows` from `controller` and clamps `config_cursor`
    /// into the (possibly shrunk) selectable list. Called both from the 1s
    /// tick (`refresh`) and immediately after any action that can change the
    /// row list — a provider toggle or a submitted edit — so a just-disabled
    /// provider's header/field rows disappear in the same `handle` call
    /// instead of lagging up to a second behind.
    async fn rebuild_config_rows(&mut self, controller: &AppController) {
        self.config_rows = build_config_rows(controller).await;
        let selectable_len = self.selectable_indices().len();
        self.config_cursor = clamp_cursor(self.config_cursor, selectable_len);
    }

    /// Reset per-tab view state when switching tabs, so every tab is entered
    /// fresh. Clears sessions_cursor, detail, config_cursor, and config_error.
    /// Leaves `editing` alone (it's guaranteed None since global keys are gated).
    fn reset_tab_state(&mut self) {
        self.sessions_cursor = 0;
        self.detail = None;
        self.config_cursor = 0;
        self.config_error = None;
    }

    pub async fn handle(&mut self, key: Key, controller: &AppController) {
        if self.editing.is_some() {
            self.handle_editing_key(key, controller).await;
            return;
        }
        if self.handle_global_key(key, controller).await {
            return;
        }
        match self.active {
            2 => self.handle_sessions_key(key),
            3 => self.handle_config_key(key, controller).await,
            _ => {}
        }
    }

    /// Global keybinds, active only while nothing is being edited. Returns
    /// `true` if `key` was consumed.
    async fn handle_global_key(&mut self, key: Key, controller: &AppController) -> bool {
        match key {
            Key::Char('q') => self.exit = true,
            Key::Char('?') => self.show_options = !self.show_options,
            Key::Tab | Key::Right => {
                let new_active = (self.active + 1) % TABS.len();
                if new_active != self.active {
                    self.active = new_active;
                    self.reset_tab_state();
                }
            }
            Key::Left => {
                let new_active = (self.active + TABS.len() - 1) % TABS.len();
                if new_active != self.active {
                    self.active = new_active;
                    self.reset_tab_state();
                }
            }
            Key::Char(c) if ('1'..='4').contains(&c) => {
                let new_active = c as usize - '1' as usize;
                if new_active != self.active {
                    self.active = new_active;
                    self.reset_tab_state();
                }
            }
            Key::Char('s') if self.active == 1 => {
                let _ = controller.toggle_daemon().await;
            }
            _ => return false,
        }
        true
    }

    /// While `editing.is_some()`, every key is text input for the draft
    /// except Enter (submit) and Esc (cancel) — edit mode swallows all other
    /// input, so even `q` types into the draft instead of quitting.
    async fn handle_editing_key(&mut self, key: Key, controller: &AppController) {
        match key {
            Key::Char(c) => {
                if let Some(draft) = self.editing.as_mut() {
                    draft.push(c);
                }
            }
            Key::Space => {
                if let Some(draft) = self.editing.as_mut() {
                    draft.push(' ');
                }
            }
            Key::Backspace => {
                if let Some(draft) = self.editing.as_mut() {
                    draft.pop();
                }
            }
            Key::Enter => self.submit_edit(controller).await,
            Key::Esc => {
                self.editing = None;
                self.config_error = None;
            }
            _ => {}
        }
    }

    /// Validates and persists the current draft against the field under the
    /// cursor. On error the draft and edit mode are kept — the input stays
    /// live showing the message, so the user can fix the value. On success,
    /// `config_rows` is rebuilt immediately rather than waiting for the next
    /// tick.
    async fn submit_edit(&mut self, controller: &AppController) {
        let selectable = self.selectable_indices();
        let Some(&row_idx) = selectable.get(self.config_cursor) else {
            return;
        };
        let Some(ConfigRow::Field(field)) = self.config_rows.get(row_idx) else {
            return;
        };
        let draft = self.editing.clone().unwrap_or_default();
        match controller.set(field.key, &draft).await {
            Ok(()) => {
                self.editing = None;
                self.config_error = None;
                self.rebuild_config_rows(controller).await;
            }
            Err(e) => self.config_error = Some(e.to_string()),
        }
    }

    /// Up/Down wrap over `sessions`; Enter opens the detail view; Esc closes
    /// it. While a detail is open, only Esc is handled — every other key is
    /// ignored until the detail closes.
    fn handle_sessions_key(&mut self, key: Key) {
        if self.detail.is_some() {
            if key == Key::Esc {
                self.detail = None;
            }
            return;
        }
        match key {
            Key::Up => {
                self.sessions_cursor = if self.sessions_cursor > 0 {
                    self.sessions_cursor - 1
                } else {
                    self.sessions.len().saturating_sub(1)
                };
            }
            Key::Down => {
                self.sessions_cursor = if self.sessions_cursor + 1 < self.sessions.len() {
                    self.sessions_cursor + 1
                } else {
                    0
                };
            }
            Key::Enter if !self.sessions.is_empty() => {
                self.detail = Some(self.sessions_cursor);
            }
            _ => {}
        }
    }

    /// Up/Down wrap over the selectable rows; Enter on a `Field` row starts
    /// editing (draft = current value or empty); Space on a toggle row
    /// flips provider membership.
    async fn handle_config_key(&mut self, key: Key, controller: &AppController) {
        let selectable = self.selectable_indices();
        if selectable.is_empty() {
            return;
        }
        match key {
            Key::Up => {
                self.config_cursor = if self.config_cursor > 0 {
                    self.config_cursor - 1
                } else {
                    selectable.len() - 1
                };
            }
            Key::Down => {
                self.config_cursor = if self.config_cursor + 1 < selectable.len() {
                    self.config_cursor + 1
                } else {
                    0
                };
            }
            Key::Enter => {
                let row_idx = selectable[self.config_cursor.min(selectable.len() - 1)];
                if let ConfigRow::Field(field) = &self.config_rows[row_idx] {
                    let current = controller.get(field.key).await.unwrap_or_default();
                    self.editing = Some(current);
                    self.config_error = None;
                }
            }
            Key::Space => {
                let row_idx = selectable[self.config_cursor.min(selectable.len() - 1)];
                if self.toggle_provider_row(row_idx, controller).await {
                    // A gateway/runtime toggle can add or remove that
                    // provider's Header/Field rows (`build_config_rows` only
                    // includes them while enabled) — rebuild now instead of
                    // leaving stale rows on screen for up to a second until
                    // the next tick.
                    self.rebuild_config_rows(controller).await;
                }
            }
            _ => {}
        }
    }

    /// Flips `id` in the corresponding enabled-CSV setting. A
    /// runtime toggle also keeps `default_runtime` valid: empty set -> `""`,
    /// else the first remaining member if the current default fell out.
    /// Returns whether the toggle was actually persisted (i.e. the CSV write
    /// succeeded), so the caller only rebuilds `config_rows` on success.
    async fn toggle_provider_row(&mut self, row_idx: usize, controller: &AppController) -> bool {
        match &self.config_rows[row_idx] {
            ConfigRow::ToggleGateway { id, .. } => {
                let id = id.clone();
                let mut ids = controller.enabled_gateways().await;
                toggle_id(&mut ids, &id);
                controller.set_enabled_gateways(&ids).await.is_ok()
            }
            ConfigRow::ToggleRuntime { id, .. } => {
                let id = id.clone();
                let mut ids = controller.enabled_runtimes().await;
                toggle_id(&mut ids, &id);
                let ok = controller.set_enabled_runtimes(&ids).await.is_ok();
                if ok {
                    if ids.is_empty() {
                        let _ = controller.set_default_runtime("").await;
                    } else {
                        let current_default = controller.default_runtime().await;
                        if !ids.iter().any(|i| i == &current_default) {
                            let _ = controller.set_default_runtime(&ids[0]).await;
                        }
                    }
                }
                ok
            }
            ConfigRow::Header(_) | ConfigRow::Field(_) => false,
        }
    }

    /// Indices into `config_rows` of every selectable (non-`Header`) row, in
    /// order; `config_cursor` indexes into *this* list, not `config_rows`.
    fn selectable_indices(&self) -> Vec<usize> {
        self.config_rows
            .iter()
            .enumerate()
            .filter(|(_, r)| !matches!(r, ConfigRow::Header(_)))
            .map(|(i, _)| i)
            .collect()
    }

    /// Test-only lookup: the selectable-index (i.e. a valid `config_cursor`
    /// value) of the `Field` row for `key`.
    #[cfg(test)]
    pub(crate) fn selectable_index_of_field(&self, key: &str) -> Option<usize> {
        self.selectable_indices()
            .into_iter()
            .position(|idx| matches!(&self.config_rows[idx], ConfigRow::Field(f) if f.key == key))
    }

    /// Test-only lookup: the selectable-index of the `ToggleRuntime` row for
    /// `id`.
    #[cfg(test)]
    pub(crate) fn selectable_index_of_runtime_toggle(&self, id: &str) -> Option<usize> {
        self.selectable_indices().into_iter().position(|idx| {
            matches!(&self.config_rows[idx], ConfigRow::ToggleRuntime { id: rid, .. } if rid == id)
        })
    }

    /// Test-only lookup: the selectable-index of the `ToggleGateway` row for
    /// `id`.
    #[cfg(test)]
    pub(crate) fn selectable_index_of_gateway_toggle(&self, id: &str) -> Option<usize> {
        self.selectable_indices().into_iter().position(|idx| {
            matches!(&self.config_rows[idx], ConfigRow::ToggleGateway { id: rid, .. } if rid == id)
        })
    }
}

/// Clamp a cursor into `0..len`, pinning to `0` when the list is empty.
fn clamp_cursor(cursor: usize, len: usize) -> usize {
    if len == 0 {
        0
    } else {
        cursor.min(len - 1)
    }
}

/// Remove `id` from `ids` if present, else append it.
fn toggle_id(ids: &mut Vec<String>, id: &str) {
    if let Some(pos) = ids.iter().position(|s| s == id) {
        ids.remove(pos);
    } else {
        ids.push(id.to_string());
    }
}

/// Builds the Config tab's row list: `Header("General")` + general fields,
/// then per *enabled* gateway/runtime that has fields a `Header(label)` +
/// its fields, then `Header("Providers")` + a toggle row for every catalog
/// gateway/runtime (enabled or not).
async fn build_config_rows(controller: &AppController) -> Vec<ConfigRow> {
    let mut rows = vec![ConfigRow::Header("General".to_string())];
    for f in controller.general_fields() {
        rows.push(ConfigRow::Field(f));
    }

    for id in controller.enabled_gateways().await {
        if let Some(gw) = controller.gateway_descriptors().iter().find(|g| g.id == id) {
            if !gw.fields.is_empty() {
                rows.push(ConfigRow::Header(gw.label.to_string()));
                rows.extend(gw.fields.iter().map(ConfigRow::Field));
            }
        }
    }

    for id in controller.enabled_runtimes().await {
        if let Some(rt) = controller.runtime_descriptors().iter().find(|r| r.id == id) {
            if !rt.fields.is_empty() {
                rows.push(ConfigRow::Header(rt.label.to_string()));
                rows.extend(rt.fields.iter().map(ConfigRow::Field));
            }
        }
    }

    rows.push(ConfigRow::Header("Providers".to_string()));
    rows.extend(
        controller
            .gateway_descriptors()
            .iter()
            .map(|gw| ConfigRow::ToggleGateway {
                id: gw.id.to_string(),
                label: format!("{} (gateway)", gw.label),
            }),
    );
    rows.extend(
        controller
            .runtime_descriptors()
            .iter()
            .map(|rt| ConfigRow::ToggleRuntime {
                id: rt.id.to_string(),
                label: format!("{} (runtime)", rt.label),
            }),
    );

    rows
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tui::controller::controller_in;
    use crate::tui::Key;
    use std::sync::{Arc, Mutex};

    #[tokio::test]
    async fn keybinds_switch_tabs_and_gate_on_editing() {
        let dir = tempfile::tempdir().unwrap();
        let c = controller_in(dir.path()).await;
        let mut d = DashboardState::new(&c).await;
        assert_eq!(d.active, 0);
        d.handle(Key::Char('2'), &c).await;
        assert_eq!(d.active, 1);
        d.handle(Key::Tab, &c).await;
        assert_eq!(d.active, 2);
        d.handle(Key::Left, &c).await;
        assert_eq!(d.active, 1);
        d.handle(Key::Char('?'), &c).await;
        assert!(d.show_options);
        d.handle(Key::Char('q'), &c).await;
        assert!(d.exit);
        // editing gates globals:
        let mut d = DashboardState::new(&c).await;
        d.active = 3;
        d.editing = Some(String::new());
        d.handle(Key::Char('q'), &c).await;
        assert!(!d.exit);
        assert_eq!(d.editing.as_deref(), Some("q")); // char went into the draft
    }

    #[tokio::test]
    async fn s_toggles_daemon_only_on_daemon_tab() {
        let dir = tempfile::tempdir().unwrap();
        let spawns: Arc<Mutex<Vec<Vec<String>>>> = Arc::default();
        let mut c = controller_in(dir.path()).await;
        let log = spawns.clone();
        c.deps.spawn_daemon = Some(Box::new(move |cmd, _log_path| {
            log.lock().unwrap().push(cmd.to_vec());
            Ok(4242)
        }));
        for (k, v) in [
            ("discord.token", "t"),
            ("discord.app_id", "a"),
            ("discord.guild_id", "g"),
            ("workdir_root", "/repos"),
        ] {
            c.set(k, v).await.unwrap();
        }
        let mut d = DashboardState::new(&c).await;
        d.active = 0;
        d.handle(Key::Char('s'), &c).await;
        assert!(
            spawns.lock().unwrap().is_empty(),
            "s on a non-daemon tab must not spawn"
        );
        d.active = 1;
        d.handle(Key::Char('s'), &c).await;
        assert_eq!(
            spawns.lock().unwrap().len(),
            1,
            "s on the daemon tab toggles the daemon"
        );
    }

    #[tokio::test]
    async fn config_toggle_runtime_keeps_default_valid() {
        let dir = tempfile::tempdir().unwrap();
        let c = controller_in(dir.path()).await; // seeds: enabled_runtimes = native, default = native (ryuzi-only defaults)
        let mut d = DashboardState::new(&c).await;
        d.active = 3;
        // move cursor to the native runtime toggle row and Space it off:
        let idx = d.selectable_index_of_runtime_toggle("native").unwrap(); // small test helper on DashboardState
        d.config_cursor = idx;
        d.handle(Key::Space, &c).await;
        assert_eq!(c.enabled_runtimes().await, Vec::<String>::new());
        assert_eq!(c.default_runtime().await, "");
        d.handle(Key::Space, &c).await; // toggle back on
        assert_eq!(c.default_runtime().await, "native");
    }

    #[tokio::test]
    async fn config_toggle_gateway_rebuilds_rows_in_same_handle_call() {
        let dir = tempfile::tempdir().unwrap();
        let c = controller_in(dir.path()).await; // seeds: enabled_gateways = discord
        let mut d = DashboardState::new(&c).await;
        d.active = 3;
        let has_discord_rows = |d: &DashboardState| {
            d.config_rows
                .iter()
                .any(|r| matches!(r, ConfigRow::Header(h) if h == "Discord"))
        };
        assert!(
            has_discord_rows(&d),
            "discord's header/fields should be present while enabled"
        );

        // move cursor to the discord gateway toggle row and Space it off:
        let idx = d.selectable_index_of_gateway_toggle("discord").unwrap();
        d.config_cursor = idx;
        d.handle(Key::Space, &c).await;

        assert_eq!(c.enabled_gateways().await, Vec::<String>::new());
        assert!(
            !has_discord_rows(&d),
            "discord's header/field rows must disappear from config_rows in the \
             same handle() call, not up to 1s later on the next tick"
        );

        // Removing discord's header/field rows shifted every later row (incl.
        // the toggle rows themselves) up in the selectable list, so re-fetch
        // the toggle row's new position rather than reusing the stale `idx`.
        let idx = d.selectable_index_of_gateway_toggle("discord").unwrap();
        d.config_cursor = idx;
        d.handle(Key::Space, &c).await; // toggle back on
        assert!(
            has_discord_rows(&d),
            "re-enabling should also rebuild immediately"
        );
    }

    #[tokio::test]
    async fn config_edit_field_roundtrip() {
        let dir = tempfile::tempdir().unwrap();
        let c = controller_in(dir.path()).await;
        let mut d = DashboardState::new(&c).await;
        d.active = 3;
        let idx = d.selectable_index_of_field("workdir_root").unwrap();
        d.config_cursor = idx;
        d.handle(Key::Enter, &c).await; // enter edit mode (draft = current value or "")
        for ch in "/tmp/x".chars() {
            d.handle(Key::Char(ch), &c).await;
        }
        d.handle(Key::Enter, &c).await; // submit
        assert!(d.editing.is_none());
        assert_eq!(c.get("workdir_root").await.as_deref(), Some("/tmp/x"));
        // Esc cancels:
        d.handle(Key::Enter, &c).await;
        d.handle(Key::Char('z'), &c).await;
        d.handle(Key::Esc, &c).await;
        assert!(d.editing.is_none());
        assert_eq!(c.get("workdir_root").await.as_deref(), Some("/tmp/x"));
    }

    #[tokio::test]
    async fn sessions_cursor_wraps_and_detail_opens() {
        let dir = tempfile::tempdir().unwrap();
        let c = controller_in(dir.path()).await;
        c.deps
            .store
            .insert_session(ryuzi_core::Session {
                session_pk: "s1".into(),
                project_id: Some("p1".into()),
                agent_session_id: None,
                worktree_path: None,
                branch: None,
                branch_owned: false,
                title: Some("first".into()),
                status: ryuzi_core::SessionStatus::Idle,
                perm_mode: ryuzi_core::PermMode::Default,
                started_by: None,
                created_at: Some(1),
                last_active: Some(1),
                resume_attempts: 0,
                kind: ryuzi_core::SessionKind::Project,
                speaker: None,
                agent: None,
                parent_session_pk: None,
            })
            .await
            .unwrap();
        c.deps
            .store
            .insert_session(ryuzi_core::Session {
                session_pk: "s2".into(),
                project_id: Some("p1".into()),
                agent_session_id: None,
                worktree_path: None,
                branch: None,
                branch_owned: false,
                title: Some("second".into()),
                status: ryuzi_core::SessionStatus::Running,
                perm_mode: ryuzi_core::PermMode::Default,
                started_by: None,
                created_at: Some(2),
                last_active: Some(2),
                resume_attempts: 0,
                kind: ryuzi_core::SessionKind::Project,
                speaker: None,
                agent: None,
                parent_session_pk: None,
            })
            .await
            .unwrap();
        let mut d = DashboardState::new(&c).await;
        d.active = 2;
        assert_eq!(d.sessions.len(), 2);
        assert_eq!(d.sessions_cursor, 0);
        d.handle(Key::Up, &c).await; // wraps to the last session
        assert_eq!(d.sessions_cursor, 1);
        d.handle(Key::Enter, &c).await;
        assert_eq!(d.detail, Some(1));
        d.handle(Key::Esc, &c).await;
        assert_eq!(d.detail, None);
    }

    #[tokio::test]
    async fn tab_switch_resets_per_tab_view_state() {
        let dir = tempfile::tempdir().unwrap();
        let c = controller_in(dir.path()).await;
        // Seed 2 sessions
        c.deps
            .store
            .insert_session(ryuzi_core::Session {
                session_pk: "s1".into(),
                project_id: Some("p1".into()),
                agent_session_id: None,
                worktree_path: None,
                branch: None,
                branch_owned: false,
                title: Some("first".into()),
                status: ryuzi_core::SessionStatus::Idle,
                perm_mode: ryuzi_core::PermMode::Default,
                started_by: None,
                created_at: Some(1),
                last_active: Some(1),
                resume_attempts: 0,
                kind: ryuzi_core::SessionKind::Project,
                speaker: None,
                agent: None,
                parent_session_pk: None,
            })
            .await
            .unwrap();
        c.deps
            .store
            .insert_session(ryuzi_core::Session {
                session_pk: "s2".into(),
                project_id: Some("p1".into()),
                agent_session_id: None,
                worktree_path: None,
                branch: None,
                branch_owned: false,
                title: Some("second".into()),
                status: ryuzi_core::SessionStatus::Running,
                perm_mode: ryuzi_core::PermMode::Default,
                started_by: None,
                created_at: Some(2),
                last_active: Some(2),
                resume_attempts: 0,
                kind: ryuzi_core::SessionKind::Project,
                speaker: None,
                agent: None,
                parent_session_pk: None,
            })
            .await
            .unwrap();

        let mut d = DashboardState::new(&c).await;
        d.active = 2; // Sessions tab
        assert_eq!(d.sessions.len(), 2);

        // Open detail on the second session
        d.sessions_cursor = 1;
        d.detail = Some(1);
        assert_eq!(d.sessions_cursor, 1);
        assert_eq!(d.detail, Some(1));

        // Switch to Config tab (key '4')
        d.handle(Key::Char('4'), &c).await;
        assert_eq!(d.active, 3);
        assert_eq!(d.sessions_cursor, 0, "sessions_cursor should reset");
        assert_eq!(d.detail, None, "detail should reset");

        // Switch back to Sessions tab (key '3')
        d.handle(Key::Char('3'), &c).await;
        assert_eq!(d.active, 2);
        assert_eq!(d.sessions_cursor, 0, "sessions_cursor should still be 0");
        assert_eq!(d.detail, None, "detail should still be None");

        // Set sessions_cursor to 1 and press '3' (same tab) - should NOT reset
        d.sessions_cursor = 1;
        d.handle(Key::Char('3'), &c).await;
        assert_eq!(d.active, 2);
        assert_eq!(
            d.sessions_cursor, 1,
            "pressing same tab digit should NOT reset state"
        );
    }

    #[tokio::test]
    async fn config_submit_error_keeps_editing() {
        let dir = tempfile::tempdir().unwrap();
        let c = controller_in(dir.path()).await;
        let mut d = DashboardState::new(&c).await;
        d.active = 3; // Config tab

        // Find the default_perm_mode field and navigate to it
        let idx = d
            .selectable_index_of_field("default_perm_mode")
            .expect("default_perm_mode field must exist");
        d.config_cursor = idx;

        // Enter edit mode (draft = current value, which is "default")
        d.handle(Key::Enter, &c).await;
        assert!(d.editing.is_some(), "should enter edit mode");

        // Clear the draft by backspacing 7 times (len("default") = 7)
        for _ in 0..7 {
            d.handle(Key::Backspace, &c).await;
        }

        // Type "bogus" (invalid enum value)
        for ch in "bogus".chars() {
            d.handle(Key::Char(ch), &c).await;
        }
        assert_eq!(d.editing.as_deref(), Some("bogus"));

        // Submit (should fail validation)
        d.handle(Key::Enter, &c).await;

        // After failed submit, editing and error should persist
        assert!(
            d.editing.is_some(),
            "editing should persist after validation error"
        );
        assert!(
            d.config_error.is_some(),
            "config_error should be set after validation error"
        );
        assert!(
            d.config_error
                .as_ref()
                .unwrap()
                .to_lowercase()
                .contains("must be one of"),
            "error message should mention valid enum values"
        );
    }

    #[tokio::test]
    async fn config_edit_field_space_key_types_into_draft() {
        let dir = tempfile::tempdir().unwrap();
        let c = controller_in(dir.path()).await;
        let mut d = DashboardState::new(&c).await;
        d.active = 3; // Config tab
        let idx = d.selectable_index_of_field("workdir_root").unwrap();
        d.config_cursor = idx;
        d.handle(Key::Enter, &c).await; // enter edit mode
                                        // Type "x y" using Key::Char and Key::Space
        d.handle(Key::Char('x'), &c).await;
        d.handle(Key::Space, &c).await;
        d.handle(Key::Char('y'), &c).await;
        assert_eq!(d.editing.as_deref(), Some("x y"));
        d.handle(Key::Enter, &c).await; // submit
        assert!(d.editing.is_none());
        assert_eq!(c.get("workdir_root").await.as_deref(), Some("x y"));
    }
}
