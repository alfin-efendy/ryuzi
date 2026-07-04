// apps/cockpit/src-tauri/src/backdrop.rs

use serde::{Deserialize, Serialize};
use specta::Type;
use tauri::WebviewWindow;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Type)]
#[serde(rename_all = "lowercase")]
pub enum BackdropCapability {
    Mica,
    Vibrancy,
    None,
}

/// Managed state: what backdrop actually got applied at startup.
pub struct BackdropState(pub BackdropCapability);

/// Mica needs Windows 11 (build 22000+); older builds get opaque. The version
/// gate here IS the capability check — Tauri returns Ok from set_effects even
/// when the OS refuses the effect, so a failed apply is undetectable.
///
/// Only called from the `windows`-gated branch of `apply_backdrop`; kept
/// cross-platform (and exercised by the tests below) so the build-number
/// threshold is verified everywhere.
#[cfg_attr(not(windows), allow(dead_code))]
pub fn windows_capability(build: u32) -> BackdropCapability {
    if build >= 22000 {
        BackdropCapability::Mica
    } else {
        BackdropCapability::None
    }
}

pub fn apply_backdrop(window: &WebviewWindow) -> BackdropCapability {
    #[cfg(windows)]
    {
        use tauri::window::{Effect, EffectsBuilder};
        let build = windows_version::OsVersion::current().build;
        let cap = windows_capability(build);
        if cap == BackdropCapability::Mica {
            let _ = window.set_effects(EffectsBuilder::new().effect(Effect::Mica).build());
        }
        cap
    }
    #[cfg(target_os = "macos")]
    {
        use tauri::window::{Effect, EffectState, EffectsBuilder};
        let _ = window.set_effects(
            EffectsBuilder::new()
                .effect(Effect::UnderWindowBackground)
                .state(EffectState::FollowsWindowActiveState)
                .build(),
        );
        BackdropCapability::Vibrancy
    }
    #[cfg(not(any(windows, target_os = "macos")))]
    {
        let _ = window;
        BackdropCapability::None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn win10_build_gets_no_backdrop() {
        assert_eq!(windows_capability(19045), BackdropCapability::None);
        assert_eq!(windows_capability(21999), BackdropCapability::None);
    }

    #[test]
    fn win11_builds_get_mica() {
        assert_eq!(windows_capability(22000), BackdropCapability::Mica);
        assert_eq!(windows_capability(26100), BackdropCapability::Mica);
    }
}
