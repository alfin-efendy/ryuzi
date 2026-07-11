//! `WizardState` — the first-run wizard as a pure, terminal-free state
//! machine (rendering lives in `render.rs`).
//!
//! Two phases: pick gateways, then fill in any still-missing required
//! settings. Persistence is eager — the gateway phase's Enter writes
//! straight through `AppController` before advancing, so quitting
//! mid-fields still leaves the gateway choice saved.

use std::collections::HashSet;

use ryuzi_core::settings::ConfigField;

use crate::tui::controller::AppController;
use crate::tui::Key;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WizardPhase {
    Gateways,
    Fields,
}

pub struct WizardState {
    pub phase: WizardPhase,
    /// Resets to 0 on every phase change — each list starts with the
    /// cursor on its first row.
    pub cursor: usize,
    pub gw_sel: Vec<String>,
    pub fields: Vec<&'static ConfigField>,
    pub field_idx: usize,
    pub draft: String,
    pub error: Option<String>,
    pub done: bool,
    pub exit: bool,
}

impl WizardState {
    /// Pre-checks `gw_sel` from the persisted `enabled_gateways` setting.
    pub async fn new(controller: &AppController) -> Self {
        let gw_sel = controller.enabled_gateways().await;
        Self {
            phase: WizardPhase::Gateways,
            cursor: 0,
            gw_sel,
            fields: Vec::new(),
            field_idx: 0,
            draft: String::new(),
            error: None,
            done: false,
            exit: false,
        }
    }

    pub async fn handle(&mut self, key: Key, controller: &AppController) {
        match self.phase {
            WizardPhase::Gateways => self.handle_list(key, controller).await,
            WizardPhase::Fields => self.handle_fields(key, controller).await,
        }
    }

    async fn handle_list(&mut self, key: Key, controller: &AppController) {
        let ids: Vec<&'static str> = controller
            .gateway_descriptors()
            .iter()
            .map(|g| g.id)
            .collect();
        match key {
            Key::Up if !ids.is_empty() => {
                self.cursor = if self.cursor > 0 {
                    self.cursor - 1
                } else {
                    ids.len() - 1
                };
            }
            Key::Down if !ids.is_empty() => {
                self.cursor = if self.cursor + 1 < ids.len() {
                    self.cursor + 1
                } else {
                    0
                };
            }
            Key::Space => {
                if let Some(&id) = ids.get(self.cursor) {
                    toggle(&mut self.gw_sel, id);
                }
            }
            Key::Enter => self.confirm_gateways(controller).await,
            Key::Esc => self.exit = true,
            _ => {}
        }
    }

    /// Enter is a no-op while nothing is selected. Persists the gateway
    /// selection, then advances to Fields (or finishes immediately when
    /// nothing required is still missing).
    async fn confirm_gateways(&mut self, controller: &AppController) {
        if self.gw_sel.is_empty() {
            return;
        }
        let _ = controller.set_enabled_gateways(&self.gw_sel).await;
        let missing = controller.required_missing_fields().await;
        let ordered = order_fields(controller, &missing).await;
        self.cursor = 0;
        if ordered.is_empty() {
            self.done = true;
        } else {
            self.fields = ordered;
            self.field_idx = 0;
            self.draft.clear();
            self.error = None;
            self.phase = WizardPhase::Fields;
        }
    }

    /// Esc is deliberately a no-op here — the fields phase can only be left
    /// by completing it (Ctrl+C remains the universal exit).
    async fn handle_fields(&mut self, key: Key, controller: &AppController) {
        match key {
            Key::Char(c) => self.draft.push(c),
            Key::Space => self.draft.push(' '),
            Key::Backspace => {
                self.draft.pop();
            }
            Key::Enter => {
                let field = self.fields[self.field_idx];
                match controller.set(field.key, &self.draft).await {
                    Ok(()) => {
                        self.error = None;
                        self.draft.clear();
                        if self.field_idx + 1 < self.fields.len() {
                            self.field_idx += 1;
                        } else {
                            self.done = true;
                        }
                    }
                    Err(e) => self.error = Some(e.to_string()),
                }
            }
            _ => {}
        }
    }
}

fn toggle(sel: &mut Vec<String>, id: &str) {
    if let Some(pos) = sel.iter().position(|s| s == id) {
        sel.remove(pos);
    } else {
        sel.push(id.to_string());
    }
}

/// Re-order missing fields so gateway provider fields come before global
/// fields, preserving each group's original relative order.
async fn order_fields(
    controller: &AppController,
    missing: &[&'static ConfigField],
) -> Vec<&'static ConfigField> {
    let mut provider_keys: HashSet<&'static str> = HashSet::new();
    for id in controller.enabled_gateways().await {
        for f in controller.gateway_fields(&id) {
            provider_keys.insert(f.key);
        }
    }
    let mut provider_fields = Vec::new();
    let mut global_fields = Vec::new();
    for f in missing {
        if provider_keys.contains(f.key) {
            provider_fields.push(*f);
        } else {
            global_fields.push(*f);
        }
    }
    provider_fields.extend(global_fields);
    provider_fields
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tui::controller::controller_in;

    #[tokio::test]
    async fn wizard_flow_gateways_fields_done() {
        let dir = tempfile::tempdir().unwrap();
        let c = controller_in(dir.path()).await;
        c.set_enabled_gateways(&[]).await.unwrap(); // start from a blank selection
        let mut w = WizardState::new(&c).await;
        assert_eq!(w.phase, WizardPhase::Gateways);

        w.handle(Key::Enter, &c).await; // Enter with empty selection: no-op
        assert_eq!(w.phase, WizardPhase::Gateways);
        w.handle(Key::Space, &c).await; // toggle discord
        w.handle(Key::Enter, &c).await;
        assert_eq!(w.phase, WizardPhase::Fields);
        assert_eq!(c.enabled_gateways().await, vec!["discord"]); // persisted eagerly
                                                                 // provider fields before globals:
        let keys: Vec<&str> = w.fields.iter().map(|f| f.key).collect();
        assert_eq!(
            keys,
            vec![
                "discord.token",
                "discord.app_id",
                "discord.guild_id",
                "workdir_root"
            ]
        );

        for value in ["tok", "app", "guild", "/repos"] {
            for ch in value.chars() {
                w.handle(Key::Char(ch), &c).await;
            }
            w.handle(Key::Enter, &c).await;
        }
        assert!(w.done);
        assert_eq!(c.get("discord.token").await.as_deref(), Some("tok"));
        assert_eq!(c.get("workdir_root").await.as_deref(), Some("/repos"));
    }

    #[tokio::test]
    async fn wizard_esc_exits_lists_but_not_fields_and_validation_keeps_draft() {
        let dir = tempfile::tempdir().unwrap();
        let c = controller_in(dir.path()).await;
        let mut w = WizardState::new(&c).await;
        w.handle(Key::Esc, &c).await;
        assert!(w.exit);
        // fields phase: force it and submit an invalid enum
        let mut w = WizardState::new(&c).await;
        w.phase = WizardPhase::Fields;
        w.fields = vec![ryuzi_core::settings::find_field("default_perm_mode").unwrap()];
        w.draft = "bogus".into();
        w.handle(Key::Enter, &c).await;
        assert_eq!(
            w.error.as_deref(),
            Some("default_perm_mode must be one of: default, acceptEdits, bypassPermissions")
        );
        assert_eq!(w.draft, "bogus"); // draft intentionally kept on error so the user can fix it
        w.handle(Key::Esc, &c).await;
        assert!(!w.exit && !w.done); // Esc is a no-op in fields phase
    }

    #[tokio::test]
    async fn gateways_enter_with_zero_missing_fields_finishes_immediately() {
        let dir = tempfile::tempdir().unwrap();
        let c = controller_in(dir.path()).await;
        for (k, v) in [
            ("discord.token", "t"),
            ("discord.app_id", "a"),
            ("discord.guild_id", "g"),
            ("workdir_root", "/r"),
        ] {
            c.set(k, v).await.unwrap();
        }
        let mut w = WizardState::new(&c).await; // discord pre-checked from seeds
        w.handle(Key::Enter, &c).await; // gateways confirmed → no missing → done
        assert!(w.done);
    }

    #[tokio::test]
    async fn wizard_fields_space_key_types_into_draft() {
        let dir = tempfile::tempdir().unwrap();
        let c = controller_in(dir.path()).await;
        c.set_enabled_gateways(&[]).await.unwrap();
        let mut w = WizardState::new(&c).await;
        // Force into Fields phase at workdir_root field
        w.phase = WizardPhase::Fields;
        w.fields = vec![ryuzi_core::settings::find_field("workdir_root").unwrap()];
        w.field_idx = 0;

        // Type "a b" using Key::Char and Key::Space
        w.handle(Key::Char('a'), &c).await;
        w.handle(Key::Space, &c).await;
        w.handle(Key::Char('b'), &c).await;
        assert_eq!(w.draft, "a b");

        // Submit the value
        w.handle(Key::Enter, &c).await;
        assert!(w.done);
        assert_eq!(c.get("workdir_root").await.as_deref(), Some("a b"));
    }
}
