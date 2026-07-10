import {
  ACCENTS,
  Button,
  Input,
  type Mode,
  Segmented,
  SettingsCard as Card,
  SettingsCardRow as CardRow,
  Switch,
  useTheme,
} from "@ryuzi/ui";
import { useEffect, useState } from "react";
import { getVersion } from "@tauri-apps/api/app";
import { disable, enable, isEnabled } from "@tauri-apps/plugin-autostart";
import { toast } from "sonner";
import { commands } from "@/bindings";
import { ModelPicker } from "@/components/ModelPicker";
import { PermissionsCard } from "@/components/PermissionsCard";
import { PERM_MODES, PROJECTS_ROOT_KEY, type UiPermMode } from "@/constants";
import { useAgent } from "@/store-agent";
import { diffLineStyle, type DiffLine } from "@/lib/diff";
import { normalizeLoopSetting } from "@/lib/loop-settings";
// Canonical brand assets (assets/brand/README.md). Explicit light/dark variants:
// the app theme is class-driven, so the prefers-color-scheme adaptive SVG can't follow it.
import wordmarkDark from "../../../../assets/brand/wordmark-dark.svg";
import wordmarkLight from "../../../../assets/brand/wordmark-light.svg";

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
    <Button variant="ghost" onClick={onPick} className="block h-auto w-full p-0 hover:bg-transparent dark:hover:bg-transparent">
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
      <div className={`mt-2.5 text-center ${selected ? "font-semibold text-foreground" : "font-medium text-muted-foreground"}`}>
        {card.label}
      </div>
    </Button>
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
          <Button
            key={a.key}
            variant="ghost"
            size="icon-xs"
            aria-label={a.label}
            title={a.label}
            onClick={() => setAccent(a.key)}
            className={`size-5 rounded-full border-border ${activeKey === a.key ? "ring-2 ring-ring ring-offset-1 ring-offset-card" : ""}`}
            style={{ background: a.primary || "oklch(0.6 0 0)" }}
          />
        ))}
        {systemAccentHex && (
          <Button
            variant="ghost"
            size="icon-xs"
            aria-label="System accent"
            title="Follow the OS accent color"
            onClick={() => setAccent("system")}
            className={`size-5 rounded-full border-border ${accent === "system" ? "ring-2 ring-ring ring-offset-1 ring-offset-card" : ""}`}
            style={{ background: systemAccentHex }}
          />
        )}
        <Input
          type="color"
          aria-label="Custom accent"
          title="Custom accent"
          value={customValue}
          onChange={(e) => setAccent({ custom: e.target.value })}
          className="size-5 cursor-pointer rounded-full border-border p-0 dark:bg-transparent"
        />
      </div>
    </CardRow>
  );
}

// ——— Agent (native) settings ———
// The two knobs that survived the Runtime menu: default model + permission
// mode, persisted in the engine settings KV via store-agent.

function AgentSection() {
  const models = useAgent((s) => s.models);
  const model = useAgent((s) => s.model);
  const permMode = useAgent((s) => s.permMode);
  const setModel = useAgent((s) => s.setModel);
  const setPermMode = useAgent((s) => s.setPermMode);

  useEffect(() => {
    void useAgent.getState().load();
  }, []);

  const permUi: UiPermMode = permMode ?? "ask";
  const permDesc = PERM_MODES.find((m) => m.id === permUi)?.desc ?? "";

  return (
    <>
      <div className="mb-4 mt-7 text-[15px] font-semibold tracking-[-0.01em]">Agent</div>
      <Card>
        <div className="flex flex-col gap-2 border-b border-border px-[18px] py-3">
          <div className="flex items-center gap-3">
            <span className="flex-1 text-[13px] font-medium">Permission mode</span>
            <Segmented
              options={PERM_MODES.map((m) => ({ id: m.id, label: m.label }))}
              value={permUi}
              onChange={(mode) => void setPermMode(mode)}
            />
          </div>
          <div className="text-right text-[11.5px] text-muted-foreground">{permDesc}</div>
        </div>
        <CardRow>
          <span className="w-[110px] shrink-0 text-[13px] font-medium">Default model</span>
          {models.length > 0 ? (
            <ModelPicker
              ariaLabel="Default model"
              variant="field"
              models={models}
              leading={[{ value: "", label: "Router default (first usable provider)" }]}
              value={model ?? ""}
              onValueChange={(v) => void setModel(v === "" ? null : v)}
            />
          ) : (
            <span className="flex-1 truncate text-xs text-muted-foreground">
              Add an enabled provider connection in Models → Providers to pick a model.
            </span>
          )}
        </CardRow>
      </Card>
    </>
  );
}

// ——— Agent loop settings ———
// Batch-3 knobs; rendered as a second card under the same "Agent" heading as
// AgentSection above (a single heading — the Settings test asserts exactly
// one "Agent" section title).

const LOOP_SETTINGS = [
  {
    key: "agent.max_provider_turns",
    label: "Max provider turns",
    desc: "Model/tool round-trips per message before pausing.",
    placeholder: "50",
    min: 1,
  },
  {
    key: "agent.auto_continue_budget",
    label: "Auto-continues",
    desc: "Automatic continues after the turn limit. 0 disables.",
    placeholder: "4",
    min: 0,
  },
] as const;

function AgentLoopCard() {
  const [values, setValues] = useState<Record<string, string>>({});
  // Last confirmed-persisted value per key, separate from `values` (which
  // tracks the live input and is mutated on every keystroke). A failed save
  // must roll back to this — not to whatever the user just typed.
  const [saved, setSaved] = useState<Record<string, string>>({});
  useEffect(() => {
    for (const s of LOOP_SETTINGS) {
      void commands.getSetting(s.key).then((res) => {
        if (res.status === "ok" && res.data) {
          setValues((cur) => ({ ...cur, [s.key]: res.data ?? "" }));
          setSaved((cur) => ({ ...cur, [s.key]: res.data ?? "" }));
        }
      });
    }
  }, []);

  const commit = async (key: string, min: number, raw: string) => {
    const normalized = normalizeLoopSetting(raw, min);
    if (normalized === null) {
      if (raw.trim() !== "") toast.error(`Enter a whole number of at least ${min}.`);
      return;
    }
    setValues((cur) => ({ ...cur, [key]: normalized }));
    const res = await commands.setSetting(key, normalized);
    if (res.status === "error") {
      setValues((cur) => ({ ...cur, [key]: saved[key] ?? "" }));
      toast.error("Couldn't save setting: " + res.error.message);
    } else {
      setSaved((cur) => ({ ...cur, [key]: normalized }));
    }
  };

  return (
    <Card className="mt-3">
      {LOOP_SETTINGS.map((s) => (
        <div key={s.key} className="flex items-center gap-3.5 border-b border-border px-[18px] py-4 last:border-b-0">
          <div className="min-w-0 flex-1">
            <div className="text-[13.5px] font-semibold">{s.label}</div>
            <div className="mt-0.5 text-[12.5px] text-muted-foreground">{s.desc}</div>
          </div>
          <Input
            aria-label={s.label}
            inputMode="numeric"
            placeholder={s.placeholder}
            value={values[s.key] ?? ""}
            onChange={(e) => setValues((cur) => ({ ...cur, [s.key]: e.target.value }))}
            onBlur={(e) => void commit(s.key, s.min, e.target.value)}
            onKeyDown={(e) => {
              if (e.key === "Enter") void commit(s.key, s.min, e.currentTarget.value);
            }}
            className="w-24 text-right tabular-nums"
          />
        </div>
      ))}
    </Card>
  );
}

// ——— View ———

export function SettingsView() {
  const mode = useTheme((s) => s.mode);
  const setMode = useTheme((s) => s.setMode);
  const transparency = useTheme((s) => s.transparency);
  const setTransparency = useTheme((s) => s.setTransparency);

  const [version, setVersion] = useState<string | null>(null);
  useEffect(() => {
    getVersion()
      .then(setVersion)
      .catch(() => setVersion(null));
  }, []);

  const [openAtLogin, setOpenAtLogin] = useState<boolean | null>(null);
  useEffect(() => {
    let cancelled = false;
    isEnabled()
      .then((v) => {
        if (!cancelled) setOpenAtLogin(v);
      })
      .catch(() => {
        if (!cancelled) setOpenAtLogin(false);
      });
    return () => {
      cancelled = true;
    };
  }, []);

  const toggleOpenAtLogin = () => {
    if (openAtLogin === null) return; // still loading
    const next = !openAtLogin;
    setOpenAtLogin(next);
    void (next ? enable() : disable()).catch((e) => {
      setOpenAtLogin(!next); // revert to the real state
      toast.error(`Open at login failed: ${e instanceof Error ? e.message : String(e)}`);
    });
  };

  const [projectsRoot, setProjectsRoot] = useState("");
  useEffect(() => {
    void commands.getSetting(PROJECTS_ROOT_KEY).then((res) => {
      if (res.status === "ok") setProjectsRoot(res.data ?? "");
    });
  }, []);

  const pickProjectsRoot = async () => {
    const dir = await commands.pickDirectory();
    if (!dir) return;
    setProjectsRoot(dir);
    const res = await commands.setSetting(PROJECTS_ROOT_KEY, dir);
    if (res.status === "error") toast.error("Couldn't save projects folder: " + res.error.message);
  };

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

        <AgentSection />

        <AgentLoopCard />

        <div className="mb-4 mt-7 text-[15px] font-semibold tracking-[-0.01em]">System</div>

        <Card>
          <div className="flex items-center gap-3.5 px-[18px] py-4">
            <div className="flex-1">
              <div className="text-[13.5px] font-semibold">Open at login</div>
              <div className="mt-0.5 text-[12.5px] text-muted-foreground">Start Cockpit when you sign in.</div>
            </div>
            <Switch on={openAtLogin === true} onToggle={toggleOpenAtLogin} label="Open at login" />
          </div>
        </Card>

        <Card className="mt-3">
          <div className="flex items-center gap-3.5 px-[18px] py-4">
            <div className="min-w-0 flex-1">
              <div className="text-[13.5px] font-semibold">Projects folder</div>
              <div className="mt-0.5 truncate text-[12.5px] text-muted-foreground">
                {projectsRoot || "Default destination for projects cloned from a URL."}
              </div>
            </div>
            <Button variant="outline" onClick={() => void pickProjectsRoot()}>
              Browse
            </Button>
          </div>
        </Card>

        <PermissionsCard />

        <div className="mb-4 mt-7 text-[15px] font-semibold tracking-[-0.01em]">About</div>

        <Card>
          <div className="flex items-center gap-3.5 px-[18px] py-4">
            <div className="flex-1">
              <img src={wordmarkLight} alt="ryuzi" className="h-5 dark:hidden" />
              <img src={wordmarkDark} alt="ryuzi" className="hidden h-5 dark:block" />
              <div className="mt-1.5 text-[12.5px] text-muted-foreground">
                Cockpit{version ? ` v${version}` : ""} — drive the Ryuzi agent from chat and terminal.
              </div>
            </div>
          </div>
        </Card>
      </div>
    </div>
  );
}
