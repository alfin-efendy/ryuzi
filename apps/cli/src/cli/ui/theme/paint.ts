import { colorEnabled } from "./colors";

const TONES = ["accent", "signature", "ok", "warn", "bad", "dim"] as const;
type Tone = (typeof TONES)[number];

const CODE: Record<Tone, number> = { accent: 36, signature: 35, ok: 32, warn: 33, bad: 31, dim: 90 };

export function paint(text: string, tone: Tone, opts?: { bold?: boolean }): string {
  if (!colorEnabled()) return text;
  const seq = (opts?.bold ? "1;" : "") + CODE[tone];
  return `\x1b[${seq}m${text}\x1b[0m`;
}
