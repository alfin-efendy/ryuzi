import { useEffect, useState } from "react";

/** Current epoch ms, re-rendering every `intervalMs` while `active` is true and
 *  frozen otherwise, so idle views never tick. Callers gate `active` on whether
 *  something is actually running (a live turn, a running agent run). */
export function useNow(active: boolean, intervalMs = 1000): number {
  const [now, setNow] = useState(() => Date.now());
  useEffect(() => {
    if (!active) return;
    setNow(Date.now());
    const id = setInterval(() => setNow(Date.now()), intervalMs);
    return () => clearInterval(id);
  }, [active, intervalMs]);
  return now;
}
