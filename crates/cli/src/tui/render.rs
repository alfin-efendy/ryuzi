//! Pure, synchronous ratatui rendering for the wizard and dashboard state
//! machines.
//!
//! `draw_wizard`/`draw_dashboard` never touch the controller or do I/O —
//! every bit of async-fetched display data (daemon state, log tail, env
//! check, missing keys, enabled provider sets, current field values) is
//! pre-gathered once per frame into `RenderCtx` by the event loop.

use std::collections::HashMap;

use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;
use ratatui::Frame;

use ryuzi_core::daemon_status::DaemonState;
use ryuzi_core::settings::{all_fields, CATALOG};

use crate::detect::Detected;
use crate::tui::controller::AppController;
use crate::tui::dashboard::{ConfigRow, DashboardState};
use crate::tui::theme::{self, Tone};
use crate::tui::widgets;
use crate::tui::wizard::{WizardPhase, WizardState};

/// Pre-fetched display data for one redraw. Gathered once per frame (event
/// loop tick) so `draw_wizard`/`draw_dashboard` stay sync and pure.
pub struct RenderCtx {
    pub daemon: DaemonState,
    /// Last 8 non-empty `daemon.log` lines.
    pub logs_tail: Vec<String>,
    /// Environment probe results: `(git, claude)` binaries.
    pub env: (Detected, Detected),
    pub missing: Vec<&'static str>,
    pub enabled_gateways: Vec<String>,
    pub enabled_runtimes: Vec<String>,
    /// Every schema field's current value (persisted or schema default; ""
    /// when truly unset) — the Config tab's field rows.
    pub field_values: HashMap<&'static str, String>,
}

impl RenderCtx {
    pub async fn gather(controller: &AppController) -> Self {
        let daemon = controller.daemon();
        let logs = controller.logs();
        let start = logs.len().saturating_sub(8);
        let logs_tail = logs[start..].to_vec();
        let env = controller.check_env();
        let missing = controller.missing_required().await;
        let enabled_gateways = controller.enabled_gateways().await;
        let enabled_runtimes = controller.enabled_runtimes().await;
        let mut field_values = HashMap::new();
        for f in all_fields() {
            let value = controller.get(f.key).await.unwrap_or_default();
            field_values.insert(f.key, value);
        }
        Self {
            daemon,
            logs_tail,
            env,
            missing,
            enabled_gateways,
            enabled_runtimes,
            field_values,
        }
    }
}

// ---------------------------------------------------------------------
// Wizard
// ---------------------------------------------------------------------

/// `r ryuzi · setup` + the phase's panel (list or field prompt).
pub fn draw_wizard(frame: &mut Frame, state: &WizardState, _ctx: &RenderCtx) {
    let area = frame.area();
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .margin(1)
        .constraints([Constraint::Length(1), Constraint::Min(0)])
        .split(area);

    frame.render_widget(Paragraph::new(brand_line(" ryuzi · setup")), chunks[0]);

    match state.phase {
        WizardPhase::Gateways => {
            let items: Vec<(&str, &str, &str)> = CATALOG
                .gateways
                .iter()
                .map(|g| (g.id, g.label, g.description))
                .collect();
            draw_wizard_list(
                frame,
                chunks[1],
                "Choose gateways",
                &items,
                &state.gw_sel,
                state.cursor,
                None,
            );
        }
        WizardPhase::Runtimes => {
            let items: Vec<(&str, &str, &str)> = CATALOG
                .runtimes
                .iter()
                .map(|r| (r.id, r.label, r.description))
                .collect();
            draw_wizard_list(
                frame,
                chunks[1],
                "Choose runtimes",
                &items,
                &state.rt_sel,
                state.cursor,
                Some(&state.detected),
            );
        }
        WizardPhase::Fields => draw_wizard_fields(frame, chunks[1], state),
    }
}

/// Multi-select list: `{caret|"  "}[x] {label} — {description}  {right}`
/// per row; `right` (runtime detect string) only on the runtimes list.
#[allow(clippy::too_many_arguments)]
fn draw_wizard_list(
    frame: &mut Frame,
    area: Rect,
    title: &str,
    items: &[(&str, &str, &str)],
    selected: &[String],
    cursor: usize,
    detected: Option<&HashMap<String, String>>,
) {
    let block = widgets::panel(title, false);
    let inner = block.inner(area);
    frame.render_widget(block, area);

    let sym = theme::symbols();
    let mut lines = vec![
        Line::from(Span::styled(
            "Space toggles · Enter continues · Esc cancels · pick at least one",
            theme::tone(Tone::Dim),
        )),
        Line::from(""),
    ];
    for (i, (id, label, description)) in items.iter().enumerate() {
        let is_cursor = i == cursor;
        let prefix = if is_cursor {
            format!("{} ", sym.caret)
        } else {
            "  ".to_string()
        };
        let checked = if selected.iter().any(|s| s == id) {
            "x"
        } else {
            " "
        };
        let mut text = format!("{prefix}[{checked}] {label}");
        if !description.is_empty() {
            text.push_str(&format!(" — {description}"));
        }
        if let Some(map) = detected {
            let right = map.get(*id).cloned().unwrap_or_else(|| "…".to_string());
            text.push_str(&format!("  {right}"));
        }
        let style = if is_cursor {
            theme::tone(Tone::Signature)
        } else {
            theme::tone(Tone::Text)
        };
        lines.push(Line::from(Span::styled(text, style)));
    }
    frame.render_widget(Paragraph::new(lines), inner);
}

/// `Settings ({i}/{n})` panel: label, dim help `(e.g. example)`, `{caret}
/// {draft}` (masked one `•` per char when secret), optional bad-toned error.
fn draw_wizard_fields(frame: &mut Frame, area: Rect, state: &WizardState) {
    let title = format!("Settings ({}/{})", state.field_idx + 1, state.fields.len());
    let block = widgets::panel(&title, false);
    let inner = block.inner(area);
    frame.render_widget(block, area);

    let field = state.fields[state.field_idx];
    let mut help = field.help.to_string();
    if let Some(example) = field.example {
        help.push_str(&format!("  (e.g. {example})"));
    }
    let masked = mask_if_secret(&state.draft, field.secret);
    let sym = theme::symbols();
    let mut lines = vec![
        Line::from(field.label),
        Line::from(Span::styled(help, theme::tone(Tone::Dim))),
        Line::from(""),
        Line::from(format!("{} {masked}", sym.caret)),
    ];
    if let Some(err) = &state.error {
        lines.push(Line::from(Span::styled(
            format!("{} {err}", sym.bad),
            theme::tone(Tone::Bad),
        )));
    }
    frame.render_widget(Paragraph::new(lines), inner);
}

// ---------------------------------------------------------------------
// Dashboard
// ---------------------------------------------------------------------

/// `r ryuzi` + tab bar, the active tab's body, the options overlay (when
/// toggled), and the status bar.
pub fn draw_dashboard(frame: &mut Frame, state: &DashboardState, ctx: &RenderCtx) {
    let area = frame.area();
    let options_height = if state.show_options { 9 } else { 0 };
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .margin(1)
        .constraints([
            Constraint::Length(1), // brand
            Constraint::Length(1), // tab bar
            Constraint::Length(1), // spacer
            Constraint::Min(3),    // body
            Constraint::Length(options_height),
            Constraint::Length(1), // status bar
        ])
        .split(area);

    frame.render_widget(Paragraph::new(brand_line(" ryuzi")), chunks[0]);
    frame.render_widget(Paragraph::new(widgets::tab_bar(state.active)), chunks[1]);

    match state.active {
        0 => draw_status_tab(frame, chunks[3], state, ctx),
        1 => draw_daemon_tab(frame, chunks[3], ctx),
        2 => draw_sessions_tab(frame, chunks[3], state),
        3 => draw_config_tab(frame, chunks[3], state, ctx),
        _ => {}
    }

    if state.show_options {
        draw_options_overlay(frame, chunks[4]);
    }

    let hints = hints_for(state.active, ctx.daemon.running);
    frame.render_widget(Paragraph::new(widgets::status_bar(&hints)), chunks[5]);
}

/// `SERVICES` (daemon/discord dots) + `SESSIONS` (active/total) +
/// `ENVIRONMENT` (git/claude) + conditional `ACTION NEEDED`.
fn draw_status_tab(frame: &mut Frame, area: Rect, state: &DashboardState, ctx: &RenderCtx) {
    let has_action = !ctx.missing.is_empty();
    let mut constraints = vec![
        Constraint::Length(3),
        Constraint::Length(3),
        Constraint::Length(3),
    ];
    if has_action {
        // 2 content rows + borders: the missing-keys line routinely runs
        // past 76 columns, so it needs to wrap instead of clipping mid-word.
        constraints.push(Constraint::Length(4));
    }
    constraints.push(Constraint::Min(0));
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints(constraints)
        .split(area);

    let d = &ctx.daemon;

    let block = widgets::panel("Services", false);
    let inner = block.inner(chunks[0]);
    frame.render_widget(block, chunks[0]);
    let line = Line::from(vec![
        Span::raw("Daemon   "),
        widgets::status_dot(d.running, if d.running { "running" } else { "stopped" }),
        Span::raw("    Discord  "),
        widgets::status_dot(d.running, if d.running { "connected" } else { "—" }),
    ]);
    frame.render_widget(Paragraph::new(line), inner);

    let block = widgets::panel("Sessions", false);
    let inner = block.inner(chunks[1]);
    frame.render_widget(block, chunks[1]);
    let active_count = state
        .sessions
        .iter()
        .filter(|s| s.status == "running")
        .count();
    let line = Line::from(vec![
        Span::styled(active_count.to_string(), theme::tone(Tone::Text)),
        Span::styled(
            format!(" active / {} total", state.sessions.len()),
            theme::tone(Tone::Dim),
        ),
    ]);
    frame.render_widget(Paragraph::new(line), inner);

    let block = widgets::panel("Environment", false);
    let inner = block.inner(chunks[2]);
    frame.render_widget(block, chunks[2]);
    let (git, claude) = &ctx.env;
    let sym = theme::symbols();
    let line = Line::from(vec![
        Span::raw("git "),
        Span::styled(
            if git.found { sym.ok } else { "…" },
            theme::tone(if git.found { Tone::Ok } else { Tone::Dim }),
        ),
        Span::raw("   claude "),
        Span::styled(
            if claude.found { sym.ok } else { "…" },
            theme::tone(if claude.found { Tone::Ok } else { Tone::Dim }),
        ),
    ]);
    frame.render_widget(Paragraph::new(line), inner);

    if has_action {
        let block = widgets::panel("Action needed", true);
        let inner = block.inner(chunks[3]);
        frame.render_widget(block, chunks[3]);
        let text = format!(
            "{} missing settings: {} — open Config (4)",
            sym.warn,
            ctx.missing.join(", ")
        );
        frame.render_widget(
            Paragraph::new(Span::styled(text, theme::tone(Tone::Warn)))
                .wrap(ratatui::widgets::Wrap { trim: true }),
            inner,
        );
    }
}

/// `DAEMON` (dot/`connecting…` + uptime + optional error) + `LOGS` (last 8
/// lines or `(none)`).
fn draw_daemon_tab(frame: &mut Frame, area: Rect, ctx: &RenderCtx) {
    let daemon_height = if ctx.daemon.last_error.is_some() {
        4
    } else {
        3
    };
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Length(daemon_height), Constraint::Min(3)])
        .split(area);

    let block = widgets::panel("Daemon", true);
    let inner = block.inner(chunks[0]);
    frame.render_widget(block, chunks[0]);
    let d = &ctx.daemon;
    let sym = theme::symbols();
    let indicator = if d.starting {
        Span::styled(format!("{} connecting…", sym.dot), theme::tone(Tone::Warn))
    } else {
        widgets::status_dot(d.running, if d.running { "running" } else { "stopped" })
    };
    let mut lines = vec![Line::from(vec![
        indicator,
        Span::styled(
            format!("    uptime {}", uptime(d.started_at)),
            theme::tone(Tone::Dim),
        ),
    ])];
    if let Some(err) = &d.last_error {
        lines.push(Line::from(Span::styled(
            format!("error {err}"),
            theme::tone(Tone::Bad),
        )));
    }
    frame.render_widget(Paragraph::new(lines), inner);

    let block = widgets::panel("Logs", false);
    let inner = block.inner(chunks[1]);
    frame.render_widget(block, chunks[1]);
    let lines: Vec<Line> = if ctx.logs_tail.is_empty() {
        vec![Line::from(Span::styled("(none)", theme::tone(Tone::Dim)))]
    } else {
        ctx.logs_tail
            .iter()
            .map(|l| Line::from(l.as_str()))
            .collect()
    };
    frame.render_widget(Paragraph::new(lines), inner);
}

/// The sessions list (`▌ ` marker, title padded 28, status badge) or its
/// empty state; a detail panel replaces the list while one is open.
fn draw_sessions_tab(frame: &mut Frame, area: Rect, state: &DashboardState) {
    if let Some(idx) = state.detail {
        if let Some(row) = state.sessions.get(idx) {
            let title = row
                .title
                .clone()
                .unwrap_or_else(|| short_pk(&row.session_pk));
            let block = widgets::panel(&title, true);
            let inner = block.inner(area);
            frame.render_widget(block, area);
            let lines = vec![
                Line::from(Span::styled(
                    format!(
                        "project {} · {} · by {}",
                        row.project_id,
                        row.status,
                        row.started_by.clone().unwrap_or_else(|| "?".to_string())
                    ),
                    theme::tone(Tone::Dim),
                )),
                Line::from(""),
                Line::from("(no output captured)"),
            ];
            frame.render_widget(Paragraph::new(lines), inner);
            return;
        }
    }

    if state.sessions.is_empty() {
        let block = widgets::panel("Sessions", false);
        let inner = block.inner(area);
        frame.render_widget(block, area);
        frame.render_widget(
            Paragraph::new(Span::styled(
                "no sessions yet — start the daemon and run from Discord",
                theme::tone(Tone::Dim),
            )),
            inner,
        );
        return;
    }

    let block = widgets::panel("Sessions", true);
    let inner = block.inner(area);
    frame.render_widget(block, area);
    let sym = theme::symbols();
    let cursor = state.sessions_cursor;
    let lines: Vec<Line> = state
        .sessions
        .iter()
        .enumerate()
        .map(|(i, row)| {
            let selected = i == cursor;
            let marker = if selected {
                format!("{} ", sym.marker)
            } else {
                "  ".to_string()
            };
            let title = row
                .title
                .clone()
                .unwrap_or_else(|| short_pk(&row.session_pk));
            let title_style = if selected {
                theme::tone(Tone::Text)
            } else {
                theme::tone(Tone::Dim)
            };
            let badge_tone = if row.status == "running" {
                Tone::Ok
            } else {
                Tone::Dim
            };
            Line::from(vec![
                Span::styled(marker, theme::tone(Tone::Signature)),
                Span::styled(format!("{title:<28}"), title_style),
                Span::styled(format!(" {}", row.status), theme::tone(badge_tone)),
            ])
        })
        .collect();
    frame.render_widget(Paragraph::new(lines), inner);
}

/// `GENERAL`/provider-group/`PROVIDERS` sections as uppercase header lines,
/// field rows (`label` padded to `label_col_width` + value/mask/`(unset)`,
/// dim help when selected), toggle rows (`[x] {label}`), an error line, and
/// the footer hint — all inside a single `CONFIG` panel.
fn draw_config_tab(frame: &mut Frame, area: Rect, state: &DashboardState, ctx: &RenderCtx) {
    let block = widgets::panel("Config", true);
    let inner = block.inner(area);
    frame.render_widget(block, area);

    // A fixed 22-column label pad would assume every label fits in 22
    // columns; it doesn't — "Default permission mode" (23), "Attachment
    // allowed hosts" (24), "Attachment allowed extensions" (29), and a few
    // `Auto-update ...` labels all overflow it, which would run the value
    // straight into the label with no gap (e.g. "Default permission
    // modedefault"). Instead, size the label column to the longest label
    // actually present this render (+2 for a gap), with 22 as the floor so
    // short label sets keep the compact layout.
    let label_col_width = state
        .config_rows
        .iter()
        .filter_map(|row| match row {
            ConfigRow::Field(field) => Some(field.label.chars().count()),
            _ => None,
        })
        .max()
        .map_or(22, |max_len| max_len + 2)
        .max(22);
    // Pin the footer hint to the panel's last row: the field list (General's
    // ~18 rows alone) routinely outgrows the visible height, and the footer
    // must stay reachable rather than scrolling off with the rest of the
    // body.
    let [body_area, footer_area] = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Min(0), Constraint::Length(1)])
        .areas(inner);

    let selectable = selectable_indices(&state.config_rows);
    let selected_flat = selectable.get(state.config_cursor).copied();
    let sym = theme::symbols();

    let mut lines: Vec<Line> = Vec::new();
    for (i, row) in state.config_rows.iter().enumerate() {
        let is_selected = Some(i) == selected_flat;
        match row {
            ConfigRow::Header(label) => {
                lines.push(Line::from(Span::styled(
                    label.to_uppercase(),
                    theme::tone_bold(Tone::Signature),
                )));
            }
            ConfigRow::Field(field) => {
                let prefix = if is_selected {
                    format!("{} ", sym.caret)
                } else {
                    "  ".to_string()
                };
                let label_col = format!("{prefix}{:<label_col_width$}", field.label);
                let label_style = if is_selected {
                    theme::tone(Tone::Signature)
                } else {
                    theme::tone(Tone::Dim)
                };
                if is_selected && state.editing.is_some() {
                    let draft = state.editing.as_deref().unwrap_or("");
                    let value = mask_if_secret(draft, field.secret);
                    lines.push(Line::from(vec![
                        Span::styled(label_col, label_style),
                        Span::raw(value),
                    ]));
                } else {
                    let raw = ctx.field_values.get(field.key).cloned().unwrap_or_default();
                    let shown = if field.secret && !raw.is_empty() {
                        "••••••••".to_string()
                    } else if raw.is_empty() {
                        "(unset)".to_string()
                    } else {
                        raw
                    };
                    lines.push(Line::from(vec![
                        Span::styled(label_col, label_style),
                        Span::raw(shown),
                    ]));
                    if is_selected {
                        let mut help = format!(" {}", field.help);
                        if let Some(example) = field.example {
                            help.push_str(&format!("  (e.g. {example})"));
                        }
                        lines.push(Line::from(Span::styled(help, theme::tone(Tone::Dim))));
                    }
                }
            }
            ConfigRow::ToggleGateway { id, label } => {
                let enabled = ctx.enabled_gateways.iter().any(|e| e == id);
                lines.push(toggle_line(is_selected, enabled, label, sym));
            }
            ConfigRow::ToggleRuntime { id, label } => {
                let enabled = ctx.enabled_runtimes.iter().any(|e| e == id);
                lines.push(toggle_line(is_selected, enabled, label, sym));
            }
        }
    }

    if let Some(err) = &state.config_error {
        lines.push(Line::from(Span::styled(
            format!("{} {err}", sym.bad),
            theme::tone(Tone::Bad),
        )));
    }

    frame.render_widget(Paragraph::new(lines), body_area);
    frame.render_widget(
        Paragraph::new(Line::from(Span::styled(
            "↑↓ select · Enter edit · Space toggle provider · Esc cancel",
            theme::tone(Tone::Dim),
        ))),
        footer_area,
    );
}

fn toggle_line(selected: bool, enabled: bool, label: &str, sym: &theme::Symbols) -> Line<'static> {
    let prefix = if selected {
        format!("{} ", sym.caret)
    } else {
        "  ".to_string()
    };
    let check = if enabled { "x" } else { " " };
    let style = if selected {
        theme::tone(Tone::Signature)
    } else {
        theme::tone(Tone::Dim)
    };
    Line::from(Span::styled(format!("{prefix}[{check}] {label}"), style))
}

/// The 6 keybinding rows, `Options` rendered as body content (not a panel
/// title — see `widgets::panel`'s doc comment for why).
fn draw_options_overlay(frame: &mut Frame, area: Rect) {
    const BINDINGS: [(&str, &str); 6] = [
        ("Tab / 1-4 / arrows", "switch tabs"),
        ("s", "start / stop daemon (Daemon tab)"),
        ("Enter", "open / edit"),
        ("Esc", "back / cancel"),
        ("?", "toggle this help"),
        ("q", "quit"),
    ];
    let block = widgets::panel("", true);
    let inner = block.inner(area);
    frame.render_widget(block, area);
    let mut lines = vec![Line::from(Span::styled(
        "Options",
        theme::tone_bold(Tone::Signature),
    ))];
    for (key, desc) in BINDINGS {
        lines.push(Line::from(vec![
            Span::styled(format!("{key:<20}"), theme::tone(Tone::Signature)),
            Span::raw(format!(" {desc}")),
        ]));
    }
    frame.render_widget(Paragraph::new(lines), inner);
}

// ---------------------------------------------------------------------
// Shared helpers
// ---------------------------------------------------------------------

/// The brand identity for CLI text surfaces per `assets/brand/README.md`:
/// the glyph `r` in the signature tone followed by the bold `ryuzi` name.
fn brand_line(suffix: &str) -> Line<'static> {
    Line::from(vec![
        Span::styled("r", theme::tone_bold(Tone::Signature)),
        Span::styled(
            suffix.to_string(),
            Style::default().add_modifier(Modifier::BOLD),
        ),
    ])
}

fn mask_if_secret(draft: &str, secret: bool) -> String {
    if secret {
        "•".repeat(draft.chars().count())
    } else {
        draft.to_string()
    }
}

fn short_pk(pk: &str) -> String {
    pk.chars().take(8).collect()
}

/// `hh:mm:ss` uptime since `started_at` (ms), `—` when `None`.
fn uptime(started_at: Option<i64>) -> String {
    let Some(started_at) = started_at else {
        return "—".to_string();
    };
    let secs = ((ryuzi_core::paths::now_ms() - started_at) / 1000).max(0);
    format!(
        "{:02}:{:02}:{:02}",
        secs / 3600,
        (secs % 3600) / 60,
        secs % 60
    )
}

/// Per-tab key hints for the status bar:
/// `Tab switch  ·  ... ·  ? options  ·  q quit`.
fn hints_for(active: usize, daemon_running: bool) -> Vec<(String, String)> {
    let mut hints = vec![("Tab".to_string(), "switch".to_string())];
    match active {
        1 => hints.push((
            "s".to_string(),
            if daemon_running { "stop" } else { "start" }.to_string(),
        )),
        2 => {
            hints.push(("↑↓".to_string(), "select".to_string()));
            hints.push(("Enter".to_string(), "open".to_string()));
        }
        3 => {
            hints.push(("↑↓".to_string(), "select".to_string()));
            hints.push(("Enter".to_string(), "edit".to_string()));
        }
        _ => {}
    }
    hints.push(("?".to_string(), "options".to_string()));
    hints.push(("q".to_string(), "quit".to_string()));
    hints
}

/// Indices into `rows` of every selectable (non-`Header`) row — mirrors
/// `DashboardState::selectable_indices` (kept private there; render derives
/// its own view since it only needs the list, not the cursor semantics).
fn selectable_indices(rows: &[ConfigRow]) -> Vec<usize> {
    rows.iter()
        .enumerate()
        .filter(|(_, r)| !matches!(r, ConfigRow::Header(_)))
        .map(|(i, _)| i)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tui::controller::controller_in;

    fn buffer_text(terminal: &ratatui::Terminal<ratatui::backend::TestBackend>) -> String {
        let buf = terminal.backend().buffer();
        (0..buf.area.height)
            .map(|y| {
                (0..buf.area.width)
                    .map(|x| buf.cell((x, y)).unwrap().symbol())
                    .collect::<String>()
                    .trim_end()
                    .to_string()
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    #[tokio::test]
    #[serial_test::serial]
    async fn snapshot_wizard_gateways_phase() {
        let dir = tempfile::tempdir().unwrap();
        let c = controller_in(dir.path()).await;
        let w = WizardState::new(&c).await;
        let ctx = RenderCtx::gather(&c).await;
        let mut terminal =
            ratatui::Terminal::new(ratatui::backend::TestBackend::new(80, 24)).unwrap();
        terminal.draw(|f| draw_wizard(f, &w, &ctx)).unwrap();
        let text = buffer_text(&terminal);
        assert!(text.contains("r ryuzi · setup"));
        assert!(text.contains("CHOOSE GATEWAYS"));
        assert!(text.contains("Space toggles · Enter continues · Esc cancels · pick at least one"));
        assert!(text.contains("[x] Discord — Drive sessions from a Discord server")); // seeded pre-check
        insta::assert_snapshot!(text);
    }

    #[tokio::test]
    #[serial_test::serial]
    async fn snapshot_dashboard_all_tabs() {
        let dir = tempfile::tempdir().unwrap();
        let c = controller_in(dir.path()).await;
        let mut d = DashboardState::new(&c).await;
        let ctx = RenderCtx::gather(&c).await;
        let mut terminal =
            ratatui::Terminal::new(ratatui::backend::TestBackend::new(80, 24)).unwrap();
        for (tab, expect) in [
            (0usize, "SERVICES"),
            (1, "stopped"),
            (2, "no sessions yet — start the daemon and run from Discord"),
            (3, "GENERAL"),
        ] {
            d.active = tab;
            terminal.draw(|f| draw_dashboard(f, &d, &ctx)).unwrap();
            let text = buffer_text(&terminal);
            assert!(text.contains("r ryuzi"), "tab {tab}");
            assert!(text.contains("1 Status"), "tab {tab}");
            assert!(text.contains(expect), "tab {tab}: {text}");
            insta::assert_snapshot!(format!("dashboard_tab_{tab}"), text);
        }
        d.show_options = true;
        terminal.draw(|f| draw_dashboard(f, &d, &ctx)).unwrap();
        let text = buffer_text(&terminal);
        assert!(text.contains("Options") && text.contains("quit"));
        insta::assert_snapshot!("dashboard_options_overlay", text);
    }
}
