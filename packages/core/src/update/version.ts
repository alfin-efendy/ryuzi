export interface SemVer {
  major: number;
  minor: number;
  patch: number;
  prerelease: string[];
}

export function parseVersion(input: string): SemVer | null {
  const s = input.trim().replace(/^v/i, "");
  const m = /^(\d+)\.(\d+)\.(\d+)(?:-([0-9A-Za-z.-]+))?(?:\+[0-9A-Za-z.-]+)?$/.exec(s);
  if (!m) return null;
  return {
    major: Number(m[1]),
    minor: Number(m[2]),
    patch: Number(m[3]),
    prerelease: m[4] ? m[4].split(".") : [],
  };
}

function cmpNum(a: number, b: number): number {
  return a < b ? -1 : a > b ? 1 : 0;
}

function cmpPrerelease(a: string[], b: string[]): number {
  // A version with NO prerelease ranks above one that has a prerelease.
  if (a.length === 0 && b.length === 0) return 0;
  if (a.length === 0) return 1;
  if (b.length === 0) return -1;
  const n = Math.min(a.length, b.length);
  for (let i = 0; i < n; i++) {
    const ai = a[i]!;
    const bi = b[i]!;
    const an = /^\d+$/.test(ai);
    const bn = /^\d+$/.test(bi);
    let c: number;
    if (an && bn) c = cmpNum(Number(ai), Number(bi));
    else if (an)
      c = -1; // numeric identifiers rank below alphanumeric ones
    else if (bn) c = 1;
    else c = ai < bi ? -1 : ai > bi ? 1 : 0;
    if (c !== 0) return c;
  }
  return cmpNum(a.length, b.length);
}

export function compareVersions(a: string, b: string): number {
  const pa = parseVersion(a);
  const pb = parseVersion(b);
  if (!pa || !pb) return 0; // unparseable → treat as equal so we never claim an update
  return (
    cmpNum(pa.major, pb.major) || cmpNum(pa.minor, pb.minor) || cmpNum(pa.patch, pb.patch) || cmpPrerelease(pa.prerelease, pb.prerelease)
  );
}

export function isNewer(current: string, latest: string): boolean {
  return compareVersions(latest, current) > 0;
}
