import { commands } from "@/bindings";

/** Parent-directory listings older than this are re-fetched. */
const TTL_MS = 30_000;

type Entry = { at: number; names: Set<string> };
const cache = new Map<string, Entry>();
const inFlight = new Map<string, Promise<Entry>>();

/** Tests only: drop all cached listings. */
export function clearProbeCache(): void {
  cache.clear();
  inFlight.clear();
}

async function parentListing(sessionPk: string, parentDir: string): Promise<Entry> {
  const key = `${sessionPk}:${parentDir}`;
  const hit = cache.get(key);
  if (hit && Date.now() - hit.at < TTL_MS) return hit;
  const pending = inFlight.get(key);
  if (pending) return pending;
  const fetch = (async () => {
    const res = await commands.listDir(sessionPk, parentDir).catch(() => null);
    // Errors cache as empty: the span renders plain and self-heals after TTL.
    const files = res && res.status === "ok" ? res.data.filter((e) => !e.dir).map((e) => e.name) : [];
    const entry: Entry = { at: Date.now(), names: new Set(files) };
    cache.set(key, entry);
    inFlight.delete(key);
    return entry;
  })();
  inFlight.set(key, fetch);
  return fetch;
}

/** Whether `rel` (workdir-relative posix path) names an existing FILE in the
 *  session worktree. Probes one metadata-only listing of the parent
 *  directory, shared across sibling files and cached for TTL_MS. */
export async function workspaceFileExists(sessionPk: string, rel: string): Promise<boolean> {
  const slash = rel.lastIndexOf("/");
  const parentDir = slash >= 0 ? rel.slice(0, slash) : "";
  const name = slash >= 0 ? rel.slice(slash + 1) : rel;
  if (!name) return false;
  const listing = await parentListing(sessionPk, parentDir);
  return listing.names.has(name);
}
