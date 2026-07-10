// Bash-style ArrowUp/Down input history for the session composer. Pure — no
// React, no DOM (the caller passes caret positions) — tested directly in
// inputHistory.test.ts; SessionView only does thin keydown wiring.

import type { Row } from "@/lib/transcript";

export type HistoryState = {
  /** Index into the newest-first entries; -1 = editing the live draft. */
  index: number;
  /** Draft stashed when navigation started; restored when cycling back down. */
  pending: string;
};

export const HISTORY_IDLE: HistoryState = { index: -1, pending: "" };

/** Sent user messages, newest first, from the session transcript rows.
 *  Attachment-only user rows (whitespace text) are skipped. */
export function historyEntries(rows: Row[]): string[] {
  const out: string[] = [];
  for (let i = rows.length - 1; i >= 0; i--) {
    const r = rows[i];
    if (r.role === "user" && r.blockType === "text" && r.text.trim()) out.push(r.text);
  }
  return out;
}

/**
 * Whether an ArrowUp/ArrowDown keydown should navigate history instead of
 * moving the caret: no slash/@ autocomplete panel open, collapsed selection,
 * and the caret on the first (up) / last (down) line — or an empty field.
 */
export function shouldNavigateHistory(
  dir: "up" | "down",
  value: string,
  selectionStart: number,
  selectionEnd: number,
  popupOpen: boolean,
): boolean {
  if (popupOpen) return false;
  if (selectionStart !== selectionEnd) return false;
  if (value === "") return true;
  return dir === "up" ? !value.slice(0, selectionStart).includes("\n") : !value.slice(selectionEnd).includes("\n");
}

export type HistoryStep = { state: HistoryState; text: string };

/**
 * One history step. Null when there is nothing to do (no entries, up at the
 * oldest, down while idle). Entering history stashes the current draft as the
 * pending buffer; stepping down past the newest entry restores it.
 */
export function stepHistory(dir: "up" | "down", entries: string[], state: HistoryState, currentText: string): HistoryStep | null {
  if (dir === "up") {
    const next = state.index + 1;
    if (next >= entries.length) return null;
    return { state: { index: next, pending: state.index === -1 ? currentText : state.pending }, text: entries[next] };
  }
  if (state.index === -1) return null;
  if (state.index === 0) return { state: { ...HISTORY_IDLE }, text: state.pending };
  return { state: { index: state.index - 1, pending: state.pending }, text: entries[state.index - 1] };
}
