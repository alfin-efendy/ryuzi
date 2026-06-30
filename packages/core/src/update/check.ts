import { isNewer } from "./version";

export interface UpdateCheckResult {
  currentVersion: string;
  latestVersion: string | null;
  updateAvailable: boolean;
  tag: string | null;
}

/**
 * Check GitHub Releases for a version newer than `currentVersion`. Never throws:
 * any network error, non-OK status, or missing `tag_name` yields a "no update"
 * result so a check failure can never crash the daemon's periodic loop.
 */
export async function checkForUpdate(opts: { currentVersion: string; repo: string; fetchImpl?: typeof fetch }): Promise<UpdateCheckResult> {
  const none: UpdateCheckResult = { currentVersion: opts.currentVersion, latestVersion: null, updateAvailable: false, tag: null };
  const fetchImpl = opts.fetchImpl ?? fetch;
  const url = `https://api.github.com/repos/${opts.repo}/releases/latest`;
  try {
    const res = await fetchImpl(url, { headers: { Accept: "application/vnd.github+json", "User-Agent": "harness-router" } });
    if (!res.ok) return none;
    const body = (await res.json()) as { tag_name?: string };
    const tag = body.tag_name ?? null;
    if (!tag) return none;
    const latestVersion = tag.replace(/^v/i, "");
    return { currentVersion: opts.currentVersion, latestVersion, updateAvailable: isNewer(opts.currentVersion, latestVersion), tag };
  } catch {
    return none;
  }
}
