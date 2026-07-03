import { useTheme } from "@ryuzi/ui";
import { getCurrentWindow } from "@tauri-apps/api/window";
import { commands, events } from "../bindings";

/**
 * Resolve backdrop capability and system accent from the Rust side, then show
 * the (initially hidden) window. Every failure path degrades to opaque/neutral
 * and STILL shows the window — a broken bridge must never leave the app
 * invisible. Safe to call outside Tauri (vite preview): every bridge call is
 * wrapped, and show() failure is swallowed.
 */
export async function initShell(): Promise<void> {
  const theme = useTheme.getState();
  try {
    theme.setCapability(await commands.backdropCapability());
  } catch {
    theme.setCapability("none");
  }
  try {
    theme.setSystemAccentHex(await commands.systemAccentColor());
  } catch {
    theme.setSystemAccentHex(null);
  }
  try {
    await events.accentChangedMsg.listen((e) => {
      useTheme.getState().setSystemAccentHex(e.payload.hex);
    });
    // ColorValuesChanged occasionally fails to fire — re-read on focus.
    window.addEventListener("focus", () => {
      commands
        .systemAccentColor()
        .then((hex) => useTheme.getState().setSystemAccentHex(hex))
        .catch(() => {});
    });
  } catch {
    // non-Tauri context: no events, no problem
  }
  try {
    await getCurrentWindow().show();
  } catch {
    // non-Tauri context
  }
}
