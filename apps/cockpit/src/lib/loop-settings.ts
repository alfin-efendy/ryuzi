// Validation for the numeric agent-loop settings (Settings → Agent).
// Mirrors the backend readers: `agent.max_provider_turns` floors at 1
// (crates/core/src/settings/mod.rs usize_setting), `agent.auto_continue_budget`
// accepts 0 (= auto-continue disabled).
export function normalizeLoopSetting(raw: string, min: number): string | null {
  const trimmed = raw.trim();
  if (!/^\d+$/.test(trimmed)) return null;
  const n = Number(trimmed);
  if (!Number.isSafeInteger(n) || n < min) return null;
  return String(n);
}
