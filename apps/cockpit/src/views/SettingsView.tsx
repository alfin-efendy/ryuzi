import { type Mode, useTheme } from "@ryuzi/ui";
import { ChevronDown } from "lucide-react";
import { useState } from "react";
import { Card, CardHeader, CardRow, CardTitle } from "@/components/common/Card";
import { Switch } from "@/components/common/Switch";

// ——— Theme mode preview cards ———

type ModeCard = {
  mode: Mode;
  label: string;
  leftBg: string;
  rightBg: string;
  winBg: string;
  winHeader: string;
  winLine: string;
  accentLine: string;
};

const MODE_CARDS: ModeCard[] = [
  {
    mode: "system",
    label: "System",
    leftBg: "#c7c9cd",
    rightBg: "#3a3d42",
    winBg: "#e9eaec",
    winHeader: "#d3d5d8",
    winLine: "#b7b9bd",
    accentLine: "#7d8ea8",
  },
  {
    mode: "light",
    label: "Light",
    leftBg: "#eceef0",
    rightBg: "#eceef0",
    winBg: "#ffffff",
    winHeader: "#e6e7ea",
    winLine: "#cfd1d5",
    accentLine: "#9aa8bd",
  },
  {
    mode: "dark",
    label: "Dark",
    leftBg: "#4a4d52",
    rightBg: "#4a4d52",
    winBg: "#2b2d31",
    winHeader: "#3a3d42",
    winLine: "#565961",
    accentLine: "#7f93b3",
  },
];

function ThemeModeCard({ card, selected, onPick }: { card: ModeCard; selected: boolean; onPick: () => void }) {
  return (
    <button type="button" onClick={onPick} className="cursor-pointer border-none bg-transparent p-0 font-sans">
      <div className={`relative h-[116px] overflow-hidden rounded-lg border-2 shadow-xs ${selected ? "border-primary" : "border-border"}`}>
        <div className="absolute inset-0 flex">
          <div className="flex-1" style={{ background: card.leftBg }} />
          <div className="flex-1" style={{ background: card.rightBg }} />
        </div>
        <div
          className="absolute overflow-hidden rounded-[8px]"
          style={{ left: "16%", top: "22%", right: "12%", bottom: "12%", background: card.winBg, boxShadow: "0 4px 14px rgba(0,0,0,0.18)" }}
        >
          <div className="flex items-center gap-1 px-2" style={{ height: "26%", background: card.winHeader }}>
            <span className="h-[5px] w-[5px] rounded-full" style={{ background: card.winLine }} />
            <span className="h-[5px] w-[5px] rounded-full" style={{ background: card.winLine }} />
          </div>
          <div className="flex flex-col gap-[5px] p-2">
            <span className="h-1 w-[60%] rounded-[2px]" style={{ background: card.winLine }} />
            <span className="h-1 w-[80%] rounded-[2px]" style={{ background: card.winLine }} />
            <span className="h-1 w-[45%] rounded-[2px]" style={{ background: card.accentLine }} />
          </div>
        </div>
      </div>
      <div
        className={`mt-2.5 text-center text-[12.5px] ${selected ? "font-semibold text-foreground" : "font-medium text-muted-foreground"}`}
      >
        {card.label}
      </div>
    </button>
  );
}

// ——— Diff preview ———

const RED_BG = "color-mix(in oklab, #EF4444 15%, transparent)";
const GREEN_BG = "color-mix(in oklab, #22C55E 16%, transparent)";

type DiffRow = { no: number; text: string; bg: string; bar: string };

const DIFF_LEFT: DiffRow[] = [
  { no: 1, text: "const themePreview: ThemeConfig = {", bg: "transparent", bar: "transparent" },
  { no: 2, text: '  surface: "sidebar",', bg: RED_BG, bar: "#EF4444" },
  { no: 3, text: '  accent: "#2563eb",', bg: RED_BG, bar: "#EF4444" },
  { no: 4, text: "  contrast: 42,", bg: RED_BG, bar: "#EF4444" },
  { no: 5, text: "};", bg: "transparent", bar: "transparent" },
];

const DIFF_RIGHT: DiffRow[] = [
  { no: 1, text: "const themePreview: ThemeConfig = {", bg: "transparent", bar: "transparent" },
  { no: 2, text: '  surface: "sidebar-elevated",', bg: GREEN_BG, bar: "#22C55E" },
  { no: 3, text: '  accent: "#0ea5e9",', bg: GREEN_BG, bar: "#22C55E" },
  { no: 4, text: "  contrast: 68,", bg: GREEN_BG, bar: "#22C55E" },
  { no: 5, text: "};", bg: "transparent", bar: "transparent" },
];

function DiffColumn({ rows, className = "" }: { rows: DiffRow[]; className?: string }) {
  return (
    <div className={`min-w-0 flex-1 ${className}`}>
      {rows.map((r) => (
        <div key={r.no} className="flex" style={{ background: r.bg }}>
          <span className="w-[3px] shrink-0" style={{ background: r.bar }} />
          <span className="w-[34px] shrink-0 select-none pr-3 text-right text-code-number">{r.no}</span>
          <span className="whitespace-pre pr-3 text-code-foreground">{r.text}</span>
        </div>
      ))}
    </div>
  );
}

// ——— Theme editor cards ———

type SwatchFieldSpec = {
  field: string;
  border: string;
  circle: string;
  circleBorder: string;
  text?: string;
  hex: string;
};

type EditorSpec = {
  title: string;
  accentHex: string;
  presetSwatch: { bg: string; border: string; color: string };
  background: SwatchFieldSpec;
  foreground: SwatchFieldSpec;
  initialContrast: number;
};

const LIGHT_EDITOR: EditorSpec = {
  title: "Light theme",
  accentHex: "#339CFF",
  presetSwatch: { bg: "#fff", border: "var(--border)", color: "#2563eb" },
  background: { field: "#fff", border: "var(--border)", circle: "#fff", circleBorder: "#d4d4d4", text: "#1A1C1F", hex: "#FFFFFF" },
  foreground: { field: "var(--muted)", border: "var(--border)", circle: "#1A1C1F", circleBorder: "rgba(0,0,0,0.3)", hex: "#1A1C1F" },
  initialContrast: 45,
};

const DARK_EDITOR: EditorSpec = {
  title: "Dark theme",
  accentHex: "#0EA5E9",
  presetSwatch: { bg: "#111318", border: "rgba(255,255,255,0.14)", color: "#339CFF" },
  background: {
    field: "#0B0B0C",
    border: "rgba(255,255,255,0.14)",
    circle: "#0B0B0C",
    circleBorder: "rgba(255,255,255,0.3)",
    text: "#E8E9EC",
    hex: "#0B0B0C",
  },
  foreground: {
    field: "#17181B",
    border: "rgba(255,255,255,0.14)",
    circle: "#E8E9EC",
    circleBorder: "rgba(255,255,255,0.5)",
    text: "#E8E9EC",
    hex: "#E8E9EC",
  },
  initialContrast: 68,
};

function GhostButton({ children }: { children: string }) {
  return (
    <button
      type="button"
      className="h-[26px] cursor-pointer rounded-sm border-none bg-transparent px-2 font-sans text-[12.5px] font-medium text-muted-foreground hover:bg-accent hover:text-accent-foreground"
    >
      {children}
    </button>
  );
}

function SwatchField({ spec }: { spec: SwatchFieldSpec }) {
  return (
    <div
      className="flex h-8 w-[172px] items-center gap-2 rounded-md border px-3"
      style={{ background: spec.field, borderColor: spec.border }}
    >
      <span className="h-3.5 w-3.5 shrink-0 rounded-full border" style={{ background: spec.circle, borderColor: spec.circleBorder }} />
      <span className="min-w-0 flex-1 font-mono text-[12.5px] font-semibold" style={spec.text ? { color: spec.text } : undefined}>
        {spec.hex}
      </span>
    </div>
  );
}

function ThemeEditorCard({ spec }: { spec: EditorSpec }) {
  const transparency = useTheme((s) => s.transparency);
  const setTransparency = useTheme((s) => s.setTransparency);
  const [contrast, setContrast] = useState(spec.initialContrast);

  return (
    <Card className="mb-3">
      <CardHeader className="gap-3">
        <CardTitle>{spec.title}</CardTitle>
        <div className="flex-1" />
        <GhostButton>Import</GhostButton>
        <GhostButton>Copy theme</GhostButton>
        <button
          type="button"
          className="flex h-[34px] cursor-pointer items-center gap-2 rounded-md border border-border bg-background px-2.5 font-sans text-[13px] font-medium text-foreground hover:bg-accent"
        >
          <span
            className="flex h-[22px] w-[22px] items-center justify-center rounded-sm border font-mono text-[10px] font-bold"
            style={{ background: spec.presetSwatch.bg, borderColor: spec.presetSwatch.border, color: spec.presetSwatch.color }}
          >
            Aa
          </span>
          Calm
          <ChevronDown size={12} className="text-muted-foreground" />
        </button>
      </CardHeader>

      <CardRow>
        <span className="flex-1 text-[13px] font-medium">Accent</span>
        <div className="flex h-8 w-[172px] items-center gap-2 rounded-md pl-3 pr-1" style={{ background: spec.accentHex }}>
          <span
            className="h-3.5 w-3.5 shrink-0 rounded-full border"
            style={{ background: "rgba(255,255,255,0.55)", borderColor: "rgba(255,255,255,0.8)" }}
          />
          <span className="min-w-0 flex-1 font-mono text-[12.5px] font-semibold text-white">{spec.accentHex}</span>
        </div>
      </CardRow>

      <CardRow>
        <span className="flex-1 text-[13px] font-medium">Background</span>
        <SwatchField spec={spec.background} />
      </CardRow>

      <CardRow>
        <span className="flex-1 text-[13px] font-medium">Foreground</span>
        <SwatchField spec={spec.foreground} />
      </CardRow>

      <CardRow>
        <span className="flex-1 text-[13px] font-medium">UI font</span>
        <div className="flex h-8 w-[172px] items-center overflow-hidden rounded-md border border-border bg-muted px-3">
          <span className="min-w-0 flex-1 truncate font-mono text-xs text-muted-foreground">-apple-system, BlinkMacSystemFont</span>
        </div>
      </CardRow>

      <CardRow>
        <span className="flex-1 text-[13px] font-medium">Translucent sidebar</span>
        <Switch on={transparency} onToggle={() => setTransparency(!transparency)} size="lg" label="Translucent sidebar" />
      </CardRow>

      <CardRow className="gap-4">
        <span className="shrink-0 text-[13px] font-medium">Contrast</span>
        <input
          type="range"
          min={0}
          max={100}
          value={contrast}
          onChange={(e) => setContrast(Number(e.target.value))}
          aria-label={`${spec.title} contrast`}
          className="flex-1 cursor-pointer"
          style={{ accentColor: "var(--primary)" }}
        />
        <span className="w-7 shrink-0 text-right font-mono text-[12.5px] font-semibold">{contrast}</span>
      </CardRow>
    </Card>
  );
}

// ——— View ———

export function SettingsView() {
  const mode = useTheme((s) => s.mode);
  const setMode = useTheme((s) => s.setMode);
  const [openAtLogin, setOpenAtLogin] = useState(true);

  return (
    <div className="min-h-0 flex-1 overflow-y-auto px-8 py-7">
      <div className="mx-auto max-w-[640px]">
        <h2 className="m-0 mb-1 text-[22px] font-semibold tracking-[-0.02em]">Settings</h2>
        <p className="m-0 mb-6 text-[13px] text-muted-foreground">Appearance and app preferences.</p>

        <div className="mb-4 text-[15px] font-semibold tracking-[-0.01em]">Appearance</div>

        <div className="mb-[22px] grid grid-cols-3 gap-4">
          {MODE_CARDS.map((card) => (
            <ThemeModeCard key={card.mode} card={card} selected={mode === card.mode} onPick={() => setMode(card.mode)} />
          ))}
        </div>

        <div className="mb-[22px] overflow-hidden rounded-xl border border-border bg-code font-mono text-xs leading-[1.85] shadow-xs">
          <div className="flex">
            <DiffColumn rows={DIFF_LEFT} className="border-r border-border" />
            <DiffColumn rows={DIFF_RIGHT} />
          </div>
        </div>

        <ThemeEditorCard spec={LIGHT_EDITOR} />
        <ThemeEditorCard spec={DARK_EDITOR} />

        <Card>
          <div className="flex items-center gap-3.5 px-[18px] py-4">
            <div className="flex-1">
              <div className="text-[13.5px] font-semibold">Open at login</div>
              <div className="mt-0.5 text-[12.5px] text-muted-foreground">Resume running sessions when the app starts.</div>
            </div>
            <Switch on={openAtLogin} onToggle={() => setOpenAtLogin(!openAtLogin)} label="Open at login" />
          </div>
        </Card>
      </div>
    </div>
  );
}
