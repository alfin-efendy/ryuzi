import { create } from "zustand";

export type Mode = "light" | "dark" | "system";
export type AccentKey = "neutral" | "indigo" | "blue" | "violet" | "emerald" | "rose" | "amber";
export type Accent = AccentKey | { custom: string };

export const ACCENTS: { key: AccentKey; label: string; primary: string; primaryForeground: string }[] = [
  { key: "neutral", label: "Neutral", primary: "", primaryForeground: "" },
  { key: "indigo", label: "Indigo", primary: "oklch(0.55 0.22 277)", primaryForeground: "oklch(0.98 0 0)" },
  { key: "blue", label: "Blue", primary: "oklch(0.6 0.2 250)", primaryForeground: "oklch(0.98 0 0)" },
  { key: "violet", label: "Violet", primary: "oklch(0.55 0.25 300)", primaryForeground: "oklch(0.98 0 0)" },
  { key: "emerald", label: "Emerald", primary: "oklch(0.6 0.16 160)", primaryForeground: "oklch(0.98 0 0)" },
  { key: "rose", label: "Rose", primary: "oklch(0.62 0.22 18)", primaryForeground: "oklch(0.98 0 0)" },
  { key: "amber", label: "Amber", primary: "oklch(0.78 0.16 75)", primaryForeground: "oklch(0.2 0 0)" },
];

const KEY_MODE = "cockpit.theme.mode";
const KEY_ACCENT = "cockpit.theme.accent";
const ACCENT_PROPS = ["--primary", "--primary-foreground", "--ring", "--sidebar-primary"] as const;

export function resolveDark(mode: Mode, systemPrefersDark: boolean): boolean {
  return mode === "dark" || (mode === "system" && systemPrefersDark);
}

function hexLuminance(hex: string): number {
  const h = hex.replace("#", "");
  const r = parseInt(h.slice(0, 2), 16) / 255;
  const g = parseInt(h.slice(2, 4), 16) / 255;
  const b = parseInt(h.slice(4, 6), 16) / 255;
  return 0.2126 * r + 0.7152 * g + 0.0722 * b;
}

export function accentVars(accent: Accent): Record<string, string> {
  if (typeof accent === "object") {
    const fg = hexLuminance(accent.custom) > 0.5 ? "oklch(0.2 0 0)" : "oklch(0.98 0 0)";
    return { "--primary": accent.custom, "--primary-foreground": fg, "--ring": accent.custom, "--sidebar-primary": accent.custom };
  }
  if (accent === "neutral") return {};
  const e = ACCENTS.find((a) => a.key === accent);
  if (!e?.primary) return {};
  return { "--primary": e.primary, "--primary-foreground": e.primaryForeground, "--ring": e.primary, "--sidebar-primary": e.primary };
}

export function applyTheme(mode: Mode, accent: Accent): void {
  if (typeof document === "undefined") return;
  const root = document.documentElement;
  const sysDark = window.matchMedia("(prefers-color-scheme: dark)").matches;
  root.classList.toggle("dark", resolveDark(mode, sysDark));
  const vars = accentVars(accent);
  for (const p of ACCENT_PROPS) {
    const v = vars[p];
    if (v) root.style.setProperty(p, v);
    else root.style.removeProperty(p);
  }
}

function readMode(): Mode {
  if (typeof localStorage === "undefined") return "system";
  const m = localStorage.getItem(KEY_MODE);
  return m === "light" || m === "dark" || m === "system" ? m : "system";
}
function readAccent(): Accent {
  if (typeof localStorage === "undefined") return "neutral";
  const raw = localStorage.getItem(KEY_ACCENT);
  if (!raw) return "neutral";
  if (raw.startsWith("#")) return { custom: raw };
  return ACCENTS.some((a) => a.key === raw) ? (raw as AccentKey) : "neutral";
}
function persistAccent(a: Accent): string {
  return typeof a === "object" ? a.custom : a;
}

type ThemeState = {
  mode: Mode;
  accent: Accent;
  setMode: (m: Mode) => void;
  setAccent: (a: Accent) => void;
};

export const useTheme = create<ThemeState>((set, get) => ({
  mode: readMode(),
  accent: readAccent(),
  setMode: (mode) => {
    if (typeof localStorage !== "undefined") localStorage.setItem(KEY_MODE, mode);
    set({ mode });
    applyTheme(mode, get().accent);
  },
  setAccent: (accent) => {
    if (typeof localStorage !== "undefined") localStorage.setItem(KEY_ACCENT, persistAccent(accent));
    set({ accent });
    applyTheme(get().mode, accent);
  },
}));

let listening = false;
export function initTheme(): void {
  applyTheme(readMode(), readAccent());
  if (!listening && typeof window !== "undefined") {
    listening = true;
    window.matchMedia("(prefers-color-scheme: dark)").addEventListener("change", () => {
      const s = useTheme.getState();
      if (s.mode === "system") applyTheme("system", s.accent);
    });
  }
}
