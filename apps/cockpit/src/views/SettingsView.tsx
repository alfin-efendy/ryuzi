import { ACCENTS, type Mode, useTheme } from "@ryuzi/ui";
import { Card, CardRow } from "@/components/common/Card";
import { Switch } from "@/components/common/Switch";
import { diffLineStyle, type DiffLine } from "@/lib/diff";

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

// ——— Live diff preview ———
// Sample rows rendered with the real --diff-* tokens and the app font stack,
// so this previews exactly what the session Review tab renders.

const PREVIEW_LINES: DiffLine[] = [
  ["hunk", "", "@@ -1,4 +1,4 @@"],
  ["ctx", 1, "fn greet(name: &str) -> String {"],
  ["del", 2, '    format!("Hello {name}")'],
  ["add", 2, '    format!("Hello, {name}!")'],
  ["ctx", 3, "}"],
];

function DiffPreview() {
  return (
    <div className="mb-[22px] overflow-hidden rounded-xl border border-border bg-code font-mono text-xs leading-[1.85] shadow-xs">
      {PREVIEW_LINES.map((l, i) => {
        const s = diffLineStyle(l);
        return (
          <div key={`${i}-${l[2]}`} className="flex" style={{ background: s.bg, color: s.color }}>
            <span className="w-[34px] shrink-0 select-none pr-3 text-right text-code-number" style={{ background: s.numBg }}>
              {l[1]}
            </span>
            <span className="w-4 shrink-0 select-none" style={{ color: s.signColor }}>
              {s.sign}
            </span>
            <span className="whitespace-pre pr-3">{l[2]}</span>
          </div>
        );
      })}
    </div>
  );
}

// ——— Accent picker ———

function AccentRow() {
  const accent = useTheme((s) => s.accent);
  const setAccent = useTheme((s) => s.setAccent);
  const systemAccentHex = useTheme((s) => s.systemAccentHex);
  const activeKey = typeof accent === "object" ? "" : accent;
  const customValue = typeof accent === "object" ? accent.custom : "#4f46e5";

  return (
    <CardRow>
      <span className="flex-1 text-[13px] font-medium">Accent</span>
      <div className="flex flex-wrap items-center gap-2">
        {ACCENTS.map((a) => (
          <button
            key={a.key}
            type="button"
            aria-label={a.label}
            title={a.label}
            onClick={() => setAccent(a.key)}
            className={`h-5 w-5 cursor-pointer rounded-full border border-border ${activeKey === a.key ? "ring-2 ring-ring ring-offset-1 ring-offset-card" : ""}`}
            style={{ background: a.primary || "oklch(0.6 0 0)" }}
          />
        ))}
        {systemAccentHex && (
          <button
            type="button"
            aria-label="System accent"
            title="Follow the OS accent color"
            onClick={() => setAccent("system")}
            className={`h-5 w-5 cursor-pointer rounded-full border border-border ${accent === "system" ? "ring-2 ring-ring ring-offset-1 ring-offset-card" : ""}`}
            style={{ background: systemAccentHex }}
          />
        )}
        <input
          type="color"
          aria-label="Custom accent"
          title="Custom accent"
          value={customValue}
          onChange={(e) => setAccent({ custom: e.target.value })}
          className="h-5 w-5 cursor-pointer rounded-full border border-border bg-transparent p-0"
        />
      </div>
    </CardRow>
  );
}

// ——— View ———

export function SettingsView() {
  const mode = useTheme((s) => s.mode);
  const setMode = useTheme((s) => s.setMode);
  const transparency = useTheme((s) => s.transparency);
  const setTransparency = useTheme((s) => s.setTransparency);

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

        <DiffPreview />

        <Card className="mb-3">
          <AccentRow />
          <CardRow>
            <span className="flex-1 text-[13px] font-medium">Translucent sidebar</span>
            <Switch on={transparency} onToggle={() => setTransparency(!transparency)} size="lg" label="Translucent sidebar" />
          </CardRow>
        </Card>

        <div className="mb-4 mt-7 text-[15px] font-semibold tracking-[-0.01em]">System</div>

        <Card>
          <div className="flex items-center gap-3.5 px-[18px] py-4">
            <div className="flex-1">
              <div className="text-[13.5px] font-semibold">Open at login</div>
              <div className="mt-0.5 text-[12.5px] text-muted-foreground">Start Cockpit when you sign in.</div>
            </div>
            {/* Wired to the autostart plugin in the next task. */}
            <Switch on={false} onToggle={() => {}} label="Open at login" />
          </div>
        </Card>
      </div>
    </div>
  );
}
