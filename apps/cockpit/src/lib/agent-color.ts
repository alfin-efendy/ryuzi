// Stable per-agent color for group-chat UI: speaker bubbles (Transcript) and
// the task strip (C2) both key off an agent's display name, so this hashes
// the name into a deterministic hue instead of tracking an id->color map
// that would need to grow every time a new worker/orchestrator agent shows
// up in a session.

/** Small string hash (djb2 variant) — deterministic, no external deps. */
function hashString(s: string): number {
  let h = 5381;
  for (let i = 0; i < s.length; i++) {
    h = (h * 33) ^ s.charCodeAt(i);
  }
  return h >>> 0;
}

/**
 * Derives a stable HSL color from an agent name. Same name always yields the
 * same color within a session and across renders/reloads (pure function of
 * the string, no session-scoped counter).
 */
export function agentColor(name: string): string {
  const hue = hashString(name) % 360;
  return `hsl(${hue} 70% 55%)`;
}
