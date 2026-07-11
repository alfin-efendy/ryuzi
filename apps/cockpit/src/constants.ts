// Pure UI copy and presentation constants. Anything stateful lives in the
// domain stores backed by real IPC — nothing here is data.

// Backend settings-table key: default destination for "Clone from URL"
// (SettingsView "Projects folder"). Same storage mechanism as workdir_root.
export const PROJECTS_ROOT_KEY = "projects_root";

// Backend settings-table key: base directory git session worktrees are
// created under (SettingsView "Worktree folder"). Same raw-setting
// mechanism as PROJECTS_ROOT_KEY — empty/unset falls back to the engine's
// default app-data location.
export const WORKTREE_DIR_KEY = "worktree_dir";

export type UiPermMode = "plan" | "ask" | "edit" | "full";

export const PERM_MODES: { id: UiPermMode; label: string; desc: string }[] = [
  { id: "plan", label: "Plan", desc: "Proposes a plan first; every action needs approval." },
  { id: "ask", label: "Ask", desc: "Asks before edits and shell commands." },
  { id: "edit", label: "Edit", desc: "Edits files freely, asks before shell commands." },
  { id: "full", label: "Full", desc: "Full access — no approval prompts." },
];

// The project row stores the engine's `PermMode`; the composer speaks the UI
// four-mode vocabulary. These keep the two in sync (maps onto the engine's
// PermMode at session start).
export type CorePermMode = "default" | "acceptEdits" | "bypassPermissions" | "plan";

export function corePermToUi(mode: CorePermMode | string): UiPermMode {
  switch (mode) {
    case "plan":
      return "plan";
    case "acceptEdits":
      return "edit";
    case "bypassPermissions":
      return "full";
    default:
      return "ask";
  }
}

export function uiPermToCore(mode: UiPermMode): CorePermMode {
  switch (mode) {
    case "plan":
      return "plan";
    case "edit":
      return "acceptEdits";
    case "full":
      return "bypassPermissions";
    default:
      return "default";
  }
}

export type GatewayFsMode = "full" | "projects" | "read";

export const GW_FS_MODES: { id: GatewayFsMode; label: string; desc: string }[] = [
  { id: "full", label: "Full", desc: "Agents may read and write anywhere the daemon user can." },
  { id: "projects", label: "Projects only", desc: "Agents are sandboxed to the connected project folders." },
  { id: "read", label: "Read-only", desc: "Agents can inspect files but never write outside a worktree." },
];

// Command entries surfaced by the ⌘K palette.
export const SEARCH_COMMANDS: { id: string; label: string; keywords: string }[] = [
  { id: "new-session", label: "New session", keywords: "new session start compose" },
  { id: "toggle-terminal", label: "Toggle terminal panel", keywords: "terminal bottom panel shell" },
  { id: "toggle-right", label: "Toggle right panel", keywords: "review files right panel" },
  { id: "gateways", label: "Manage gateways", keywords: "gateway workspace ssh daemon connect" },
  { id: "models", label: "Open models", keywords: "models endpoint provider connection api key claude openai" },
  { id: "scheduler", label: "New scheduled job", keywords: "schedule cron job recurring" },
  { id: "settings", label: "Open settings", keywords: "settings appearance theme transparency" },
];

export const HOME_SUGGESTIONS = ["Fix the failing e2e suite", "Add rate limiting to the API", "Upgrade to React 20"];

// Quota bars tint by pressure: calm → amber → red as headroom shrinks.
export function quotaColor(pct: number): string {
  if (pct >= 90) return "#EF4444";
  if (pct >= 75) return "#F59E0B";
  return "#22C55E";
}

// Kiro (free tier) device sign-in + Kiro IDE import copy. Kiro doesn't
// publish a fixed free-tier quota, so this deliberately never states a
// number — only what's happening in the current step.
export const KIRO_PICKER_SUBTITLE = "Free — sign in required";
export const KIRO_SIGNIN_ACTION = "Sign in with Kiro";
export const KIRO_SIGNIN_SUBTITLE = "Free — sign in with your AWS Builder ID account. No API key needed.";
export const KIRO_DEVICE_CODE_HINT = "We opened your browser — enter this code to sign in:";
export const KIRO_WAITING_HINT = "Waiting for you to finish signing in…";
export const KIRO_IMPORT_ACTION = "Import from Kiro IDE";
export const KIRO_IMPORT_HINT = "Already signed in to the Kiro IDE on this machine? Import that sign-in instead of starting a new one.";
export const KIRO_IMPORT_SUCCESS = "Imported your Kiro sign-in";

// Keychain-status warning banner (Endpoint tab). Secrets are always encrypted
// at rest; these two strings are honest about *where* the master key that
// protects them lives when it isn't the OS keychain.
export const KEYCHAIN_FILE_FALLBACK_WARNING = "Secrets are stored in a local file, not the OS keychain.";
export const KEYCHAIN_UNAVAILABLE_WARNING = "Secrets are stored unencrypted — no OS keychain available.";

// Shown in the Add-account modal for catalog entries flagged `riskNotice`
// (providers reached through unofficial/reverse-engineered endpoints).
export const PROVIDER_RISK_NOTICE =
  "Uses your provider account through unofficial endpoints. This may violate the provider's terms and can risk account suspension.";

// Device sign-in copy for RFC 8628 device-grant providers (qwen, github-copilot).
export const DEVICE_SIGNIN_ACTION = "Sign in";
export const PROVIDER_DEVICE_SUBTITLE: Record<string, string> = {
  qwen: "Free — sign in with your Qwen account. No API key needed.",
  "github-copilot": "Sign in with your GitHub account to use your Copilot subscription.",
};

// The one (native, in-process) agent. Identity only — model/perm-mode state
// lives in store-agent. These values are the native agent's identity, defined
// only here now that the runtime concept is gone.
export const NATIVE_AGENT = { id: "native", name: "Ryuzi", color: "#7C5CFF", initial: "R" } as const;
