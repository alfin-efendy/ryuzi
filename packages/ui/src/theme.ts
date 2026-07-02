import { create } from "zustand";

export type Mode = "light" | "dark" | "system";
export type AccentKey = "neutral" | "indigo" | "blue" | "violet" | "emerald" | "rose" | "amber";
export type Accent = AccentKey | "system" | { custom: string };
export type BackdropCapability = "mica" | "vibrancy" | "none";

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
const KEY_TRANSPARENCY = "cockpit.theme.transparency";
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

export function accentVars(accent: Accent, systemAccentHex?: string | null): Record<string, string> {
  if (accent === "system") {
    return systemAccentHex ? accentVars({ custom: systemAccentHex }) : {};
  }
  if (typeof accent === "object") {
    const fg = hexLuminance(accent.custom) > 0.5 ? "oklch(0.2 0 0)" : "oklch(0.98 0 0)";
    return { "--primary": accent.custom, "--primary-foreground": fg, "--ring": accent.custom, "--sidebar-primary": accent.custom };
  }
  if (accent === "neutral") return {};
  const e = ACCENTS.find((a) => a.key === accent);
  if (!e?.primary) return {};
  return { "--primary": e.primary, "--primary-foreground": e.primaryForeground, "--ring": e.primary, "--sidebar-primary": e.primary };
}

export function resolveBackdropAttr(cap: BackdropCapability, transparency: boolean): "mica" | "vibrancy" | null {
  if (!transparency || cap === "none") return null;
  return cap;
}

export function applyBackdrop(cap: BackdropCapability, transparency: boolean): void {
  if (typeof document === "undefined") return;
  const attr = resolveBackdropAttr(cap, transparency);
  if (attr) document.documentElement.dataset.backdrop = attr;
  else delete document.documentElement.dataset.backdrop;
}

export function applyTheme(mode: Mode, accent: Accent, systemAccentHex?: string | null): void {
  if (typeof document === "undefined" || typeof window === "undefined") return;
  const root = document.documentElement;
  const sysDark = window.matchMedia("(prefers-color-scheme: dark)").matches;
  root.classList.toggle("dark", resolveDark(mode, sysDark));
  const vars = accentVars(accent, systemAccentHex);
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

export function parseAccent(raw: string | null): Accent {
  if (!raw) return "neutral";
  if (raw === "system") return "system";
  if (raw.startsWith("#")) return { custom: raw };
  return ACCENTS.some((a) => a.key === raw) ? (raw as AccentKey) : "neutral";
}

function readAccent(): Accent {
  if (typeof localStorage === "undefined") return "neutral";
  return parseAccent(localStorage.getItem(KEY_ACCENT));
}

function readTransparency(): boolean {
  if (typeof localStorage === "undefined") return true;
  return localStorage.getItem(KEY_TRANSPARENCY) !== "0";
}

function persistAccent(a: Accent): string {
  return typeof a === "object" ? a.custom : a;
}

type ThemeState = {
  mode: Mode;
  accent: Accent;
  transparency: boolean;
  capability: BackdropCapability;
  systemAccentHex: string | null;
  setMode: (m: Mode) => void;
  setAccent: (a: Accent) => void;
  setTransparency: (v: boolean) => void;
  setCapability: (c: BackdropCapability) => void;
  setSystemAccentHex: (hex: string | null) => void;
};

export const useTheme = create<ThemeState>((set, get) => ({
  mode: readMode(),
  accent: readAccent(),
  transparency: readTransparency(),
  capability: "none",
  systemAccentHex: null,
  setMode: (mode) => {
    if (typeof localStorage !== "undefined") localStorage.setItem(KEY_MODE, mode);
    set({ mode });
    applyTheme(mode, get().accent, get().systemAccentHex);
  },
  setAccent: (accent) => {
    if (typeof localStorage !== "undefined") localStorage.setItem(KEY_ACCENT, persistAccent(accent));
    set({ accent });
    applyTheme(get().mode, accent, get().systemAccentHex);
  },
  setTransparency: (transparency) => {
    if (typeof localStorage !== "undefined") localStorage.setItem(KEY_TRANSPARENCY, transparency ? "1" : "0");
    set({ transparency });
    applyBackdrop(get().capability, transparency);
  },
  setCapability: (capability) => {
    set({ capability });
    applyBackdrop(capability, get().transparency);
  },
  setSystemAccentHex: (systemAccentHex) => {
    set({ systemAccentHex });
    const s = get();
    if (s.accent === "system") applyTheme(s.mode, s.accent, systemAccentHex);
  },
}));

let listening = false;
export function initTheme(): void {
  applyTheme(readMode(), readAccent());
  if (!listening && typeof window !== "undefined") {
    listening = true;
    window.matchMedia("(prefers-color-scheme: dark)").addEventListener("change", () => {
      const s = useTheme.getState();
      if (s.mode === "system") applyTheme("system", s.accent, s.systemAccentHex);
    });
  }
}
