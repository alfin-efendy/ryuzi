use ratatui::prelude::Stylize;
use ratatui::style::{Color, Style};
use ratatui::widgets::BorderType;

/// Symbol sets for UI rendering
#[derive(Clone, Copy, Debug)]
pub struct Symbols {
    pub dot: &'static str,
    pub dot_off: &'static str,
    pub ok: &'static str,
    pub bad: &'static str,
    pub marker: &'static str,
    pub warn: &'static str,
    pub caret: &'static str,
    pub glyph: &'static str,
}

/// Unicode symbol set
const UNICODE_SYMBOLS: Symbols = Symbols {
    dot: "●",
    dot_off: "○",
    ok: "✓",
    bad: "✗",
    marker: "▌",
    warn: "⚠",
    caret: "›",
    glyph: "r",
};

/// ASCII fallback symbol set
const ASCII_SYMBOLS: Symbols = Symbols {
    dot: "*",
    dot_off: "o",
    ok: "+",
    bad: "x",
    marker: "|",
    warn: "!",
    caret: ">",
    glyph: "r",
};

/// Tones for styling
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Tone {
    Accent,
    Signature,
    Ok,
    Warn,
    Bad,
    Dim,
    Text,
    Border,
}

/// Check if unicode symbols are enabled
/// Returns false if RYUZI_ASCII environment variable is set
pub fn unicode_enabled() -> bool {
    std::env::var_os("RYUZI_ASCII").is_none()
}

/// Get the appropriate symbol set based on unicode_enabled()
pub fn symbols() -> &'static Symbols {
    if unicode_enabled() {
        &UNICODE_SYMBOLS
    } else {
        &ASCII_SYMBOLS
    }
}

/// Get border type based on RYUZI_ASCII environment variable
pub fn border_type() -> BorderType {
    if unicode_enabled() {
        BorderType::Rounded
    } else {
        BorderType::Plain
    }
}

/// Check if colors are enabled
/// Returns true if stdout is a terminal AND NO_COLOR is not set AND TERM != "dumb"
pub fn color_enabled() -> bool {
    use std::io::IsTerminal;
    std::io::stdout().is_terminal()
        && std::env::var_os("NO_COLOR").is_none()
        && std::env::var("TERM").map(|t| t != "dumb").unwrap_or(true)
}

/// Convert hex color to ratatui Color
fn hex_to_rgb(hex: &str) -> Color {
    let hex = hex.trim_start_matches('#');
    let r = u8::from_str_radix(&hex[0..2], 16).unwrap_or(0);
    let g = u8::from_str_radix(&hex[2..4], 16).unwrap_or(0);
    let b = u8::from_str_radix(&hex[4..6], 16).unwrap_or(0);
    Color::Rgb(r, g, b)
}

/// Get the color for a tone
fn tone_color(t: Tone) -> Color {
    match t {
        Tone::Accent => hex_to_rgb("#37c9e6"),
        Tone::Signature => hex_to_rgb("#ff2d95"),
        Tone::Ok => hex_to_rgb("#36f9c3"),
        Tone::Warn => hex_to_rgb("#ffb454"),
        Tone::Bad => hex_to_rgb("#ff5c69"),
        Tone::Dim => hex_to_rgb("#8b93b0"),
        Tone::Text => hex_to_rgb("#cdd4ee"),
        Tone::Border => hex_to_rgb("#272b3a"),
    }
}

/// Get style for a tone
/// Returns Style::default() when colors are disabled
pub fn tone(t: Tone) -> Style {
    if !color_enabled() {
        return Style::default();
    }
    Style::default().fg(tone_color(t))
}

/// Get bold style for a tone
/// Returns Style::default().bold() when colors are disabled
pub fn tone_bold(t: Tone) -> Style {
    if !color_enabled() {
        return Style::default().bold();
    }
    Style::default().fg(tone_color(t)).bold()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[serial_test::serial]
    fn ascii_env_flips_symbols_and_border() {
        std::env::remove_var("RYUZI_ASCII");
        assert_eq!(symbols().ok, "✓");
        assert_eq!(border_type(), ratatui::widgets::BorderType::Rounded);
        std::env::set_var("RYUZI_ASCII", "1");
        assert_eq!(symbols().ok, "+");
        assert_eq!(symbols().caret, ">");
        assert_eq!(border_type(), ratatui::widgets::BorderType::Plain);
        std::env::remove_var("RYUZI_ASCII");
    }

    #[test]
    fn tone_is_plain_when_colors_disabled() {
        // under cargo test stdout is a pipe → color_enabled() false
        assert_eq!(tone(Tone::Signature), ratatui::style::Style::default());
    }
}
