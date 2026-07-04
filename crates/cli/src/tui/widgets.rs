//! Small stateless rendering helpers shared by `render.rs`'s wizard/dashboard
//! draw functions. Rust port of the retired TypeScript `components/{panel,
//! status-bar,tab-bar,status-dot}.tsx`.

use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Padding};

use super::dashboard::TABS;
use super::theme::{self, Tone};

/// A bordered block with an uppercased title, styled by `focus` (TS parity:
/// `Panel`'s `title.toUpperCase()`). Pass an empty `title` to omit the title
/// line entirely — the options overlay uses this to dodge the uppercasing
/// and render "Options" as body content instead (TS does the same dodge).
pub fn panel(title: &str, focus: bool) -> Block<'static> {
    let border_style = theme::tone(if focus { Tone::Signature } else { Tone::Border });
    let mut block = Block::default()
        .borders(Borders::ALL)
        .border_type(theme::border_type())
        .border_style(border_style)
        .padding(Padding::horizontal(1));
    if !title.is_empty() {
        let title_style = theme::tone(if focus { Tone::Signature } else { Tone::Dim });
        block = block.title(Span::styled(title.to_uppercase(), title_style));
    }
    block
}

/// `{dot|dot_off} {label}` — TS `StatusDot`, colored ok/dim by `on`.
pub fn status_dot(on: bool, label: &str) -> Span<'static> {
    let sym = theme::symbols();
    let dot = if on { sym.dot } else { sym.dot_off };
    let text = if label.is_empty() {
        dot.to_string()
    } else {
        format!("{dot} {label}")
    };
    Span::styled(text, theme::tone(if on { Tone::Ok } else { Tone::Dim }))
}

/// `1 Status  2 Daemon  3 Sessions  4 Config` — TS `TabBar` (active bold +
/// signature, others dim; two-space gaps from TS's `marginRight={2}`).
pub fn tab_bar(active: usize) -> Line<'static> {
    let mut spans = Vec::new();
    for (i, name) in TABS.iter().enumerate() {
        if i > 0 {
            spans.push(Span::raw("  "));
        }
        let style = if i == active {
            theme::tone_bold(Tone::Signature)
        } else {
            theme::tone(Tone::Dim)
        };
        spans.push(Span::styled(format!("{} {name}", i + 1), style));
    }
    Line::from(spans)
}

/// Joins `(key, label)` hints with `  ·  ` — TS `StatusBar` + `KeyHint`
/// (bold signature key, dim label).
pub fn status_bar(hints: &[(String, String)]) -> Line<'static> {
    let mut spans = Vec::new();
    for (i, (key, label)) in hints.iter().enumerate() {
        if i > 0 {
            spans.push(Span::styled("  ·  ", theme::tone(Tone::Dim)));
        }
        spans.push(Span::styled(key.clone(), theme::tone_bold(Tone::Signature)));
        spans.push(Span::styled(format!(" {label}"), theme::tone(Tone::Dim)));
    }
    Line::from(spans)
}
