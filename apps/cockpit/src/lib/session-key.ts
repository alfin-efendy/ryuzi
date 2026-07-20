import type { Session } from "../bindings";

/** The always-present local engine. Used as the default runner id for events,
 *  optimistic sessions, and management/settings commands. */
export const LOCAL_RUNNER = "local";

/** A runner-qualified reference to a session. Across runners (each its own DB)
 *  `pk` values WILL collide, so per-session state is keyed by BOTH. */
export type SessionRef = { runnerId: string; pk: string };

/** A session stamped, client-side, with the runner that owns it. Bindings'
 *  `Session` has no runner column, so the store augments it here. */
export type UiSession = Session & { runnerId: string };

/** Composite key for the per-session state maps (transcripts, lastSeq, …) and
 *  the sibling stores' pinned/archived/readAt/todos/terminals. `::` never
 *  appears in a runner id, so the first `::` cleanly separates the two parts. */
export const sessKey = (runnerId: string, pk: string): string => `${runnerId}::${pk}`;

/** Composite key for a stamped session. */
export const sessionKey = (s: UiSession): string => sessKey(s.runnerId, s.sessionPk);

/** Composite key for a ref. */
export const refKey = (ref: SessionRef): string => sessKey(ref.runnerId, ref.pk);

/** A ref for a stamped session. */
export const refOf = (s: UiSession): SessionRef => ({ runnerId: s.runnerId, pk: s.sessionPk });

/** True when two refs point at the same session (same runner AND same pk). */
export const sameRef = (a: SessionRef | null | undefined, b: SessionRef | null | undefined): boolean =>
  !!a && !!b && a.runnerId === b.runnerId && a.pk === b.pk;

/** True when a stamped session is the one referenced by `ref`. */
export const isSession = (s: UiSession, ref: SessionRef | null | undefined): boolean =>
  !!ref && s.runnerId === ref.runnerId && s.sessionPk === ref.pk;
