import { Terminal } from "@xterm/xterm";
import { FitAddon } from "@xterm/addon-fit";
import { SearchAddon } from "@xterm/addon-search";
import "@xterm/xterm/css/xterm.css";
import { commands, events } from "@/bindings";

// App-lifetime xterm instances. Components attach/detach the cache-owned DOM
// node; the PTY is NEVER closed on unmount — only by dispose() (user closes
// the tab, shell exits, or the session is archived). Scrollback therefore
// survives drawer toggles. App restart still kills shells: PTYs are child
// processes of this app.

export type TermInstance = {
  termId: string;
  term: Terminal;
  fit: FitAddon;
  search: SearchAddon;
  container: HTMLDivElement;
  opened: boolean;
  dispose: () => void;
};

const cache = new Map<string, TermInstance>();
let copyOnSelect = false;
let onExit: ((termId: string) => void) | null = null;
// The in-flight (or resolved) listener registration. Caching the Promise — not a
// boolean — means concurrent createTerm calls all await the same registration
// (no lost first bytes), and a rejected registration resets to null so a later
// open can retry instead of leaving terminal output dead app-wide.
let listenersReady: Promise<void> | null = null;

export function setCopyOnSelect(v: boolean): void {
  copyOnSelect = v;
}

export function setOnExit(fn: (termId: string) => void): void {
  onExit = fn;
}

function ensureListeners(): Promise<void> {
  listenersReady ??= registerListeners();
  return listenersReady;
}

async function registerListeners(): Promise<void> {
  try {
    await events.termOutputMsg.listen((e) => {
      cache.get(e.payload.id)?.term.write(e.payload.data);
    });
    await events.termExitMsg.listen((e) => {
      const inst = cache.get(e.payload.id);
      if (inst) inst.term.write("\r\n\x1b[90m[process exited]\x1b[0m\r\n");
      onExit?.(e.payload.id);
    });
  } catch (err) {
    // Registration failed — reset so the next createTerm retries. Rethrow so the
    // caller (store-terms open) can surface the failure to the user.
    listenersReady = null;
    throw err;
  }
}

export function getTerm(termId: string): TermInstance | undefined {
  return cache.get(termId);
}

export async function createTerm(sessionPk: string): Promise<TermInstance | { error: string }> {
  await ensureListeners();
  // The PTY opens at a nominal size; the first attach() refits to reality.
  const res = await commands.termOpen(sessionPk, 80, 24);
  if (res.status === "error") return { error: res.error.message };
  const termId = res.data;

  const style = getComputedStyle(document.documentElement);
  const term = new Terminal({
    fontSize: 12,
    fontFamily: '"Geist Mono Variable", ui-monospace, monospace',
    cursorBlink: true,
    allowTransparency: true,
    theme: {
      background: "rgba(0,0,0,0)",
      foreground: style.getPropertyValue("--code-foreground").trim() || undefined,
    },
  });
  const fit = new FitAddon();
  term.loadAddon(fit);
  const search = new SearchAddon();
  term.loadAddon(search);

  const container = document.createElement("div");
  container.style.height = "100%";
  container.style.width = "100%";

  const data = term.onData((d) => void commands.termInput(termId, d));
  const sel = term.onSelectionChange(() => {
    if (copyOnSelect && term.hasSelection()) void navigator.clipboard.writeText(term.getSelection());
  });

  const inst: TermInstance = {
    termId,
    term,
    fit,
    search,
    container,
    opened: false,
    dispose: () => {
      cache.delete(termId);
      data.dispose();
      sel.dispose();
      term.dispose();
      void commands.termClose(termId);
    },
  };
  cache.set(termId, inst);
  return inst;
}

export function attach(inst: TermInstance, host: HTMLElement): void {
  host.appendChild(inst.container);
  if (!inst.opened) {
    // term.open() needs a measurable element, so it runs on first attach.
    inst.term.open(inst.container);
    inst.opened = true;
    void tryWebgl(inst.term);
  }
  refit(inst);
}

export function detach(inst: TermInstance): void {
  inst.container.remove();
}

export function refit(inst: TermInstance): void {
  if (!inst.opened || !inst.container.isConnected) return;
  inst.fit.fit();
  void commands.termResize(inst.termId, inst.term.cols, inst.term.rows);
}

async function tryWebgl(term: Terminal): Promise<void> {
  try {
    const { WebglAddon } = await import("@xterm/addon-webgl");
    const addon = new WebglAddon();
    addon.onContextLoss(() => addon.dispose()); // falls back to the DOM renderer
    term.loadAddon(addon);
  } catch {
    // WebGL unavailable — DOM renderer is fine.
  }
}
