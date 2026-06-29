import type React from "react";
import { Text } from "ink";
import { palette } from "../theme";

const TONE: Record<string, string> = {
  ok: palette.ok,
  warn: palette.warn,
  bad: palette.bad,
  accent: palette.accent,
  dim: palette.dim,
};

export function Badge({ tone, children }: { tone: keyof typeof TONE; children: React.ReactNode }) {
  return <Text color={TONE[tone]}>{children}</Text>;
}
