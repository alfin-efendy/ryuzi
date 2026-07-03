// apps/cockpit/src-tauri/src/accent.rs

use serde::{Deserialize, Serialize};
use specta::Type;
use tauri_specta::Event;

#[derive(Debug, Clone, Serialize, Deserialize, Type, Event)]
pub struct AccentChangedMsg {
    pub hex: String,
}

/// `#rrggbb` (alpha dropped) — the exact form theme.ts's hexLuminance parses.
pub fn rgb_to_hex(r: u8, g: u8, b: u8) -> String {
    format!("#{r:02x}{g:02x}{b:02x}")
}

#[cfg(windows)]
pub fn read_accent_hex() -> Option<String> {
    use windows::UI::ViewManagement::{UIColorType, UISettings};
    let settings = UISettings::new().ok()?;
    let c = settings.GetColorValue(UIColorType::Accent).ok()?;
    Some(rgb_to_hex(c.R, c.G, c.B))
}

#[cfg(not(windows))]
pub fn read_accent_hex() -> Option<String> {
    None
}

#[tauri::command]
#[specta::specta]
pub fn system_accent_color() -> Option<String> {
    read_accent_hex()
}

/// Subscribe to Windows accent changes and forward them to the webview.
/// The UISettings instance and its registration are intentionally leaked: the
/// subscription must live for the app's lifetime or ColorValuesChanged silently
/// stops firing. The handler runs on a background MTA thread and fires several
/// times per change — the frontend applies idempotently, so no debounce needed.
#[cfg(windows)]
pub fn spawn_accent_watcher(app: &tauri::AppHandle) {
    use tauri_specta::Event as _;
    use windows::Foundation::TypedEventHandler;
    use windows::UI::ViewManagement::UISettings;

    let Ok(settings) = UISettings::new() else { return };
    let handle = app.clone();
    let handler = TypedEventHandler::new(move |_, _| {
        if let Some(hex) = read_accent_hex() {
            let _ = AccentChangedMsg { hex }.emit(&handle);
        }
        Ok(())
    });
    if settings.ColorValuesChanged(&handler).is_ok() {
        std::mem::forget(settings);
    }
}

#[cfg(not(windows))]
pub fn spawn_accent_watcher(_app: &tauri::AppHandle) {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn formats_lowercase_rrggbb_without_alpha() {
        assert_eq!(rgb_to_hex(0, 120, 212), "#0078d4");
        assert_eq!(rgb_to_hex(255, 255, 255), "#ffffff");
        assert_eq!(rgb_to_hex(0, 0, 0), "#000000");
    }
}
