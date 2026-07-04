import { useEffect, useRef, useState } from "react";
import { Copy, Plus, Search, SquareTerminal, X } from "lucide-react";
import { useNav, clampPanelSize, BOTTOM_HEIGHT } from "@/store-nav";
import { useTerms } from "@/store-terms";
import { attach, detach, getTerm, refit, type TermInstance } from "@/lib/term-cache";
import { PanelResizeHandle } from "@/components/common/PanelResizeHandle";

const toolBtn =
  "flex h-[30px] w-[30px] cursor-pointer items-center justify-center rounded-md border-none bg-transparent text-muted-foreground hover:bg-accent hover:text-accent-foreground";

function TerminalHost({ inst, className }: { inst: TermInstance; className?: string }) {
  const ref = useRef<HTMLDivElement>(null);
  useEffect(() => {
    const host = ref.current;
    if (!host) return;
    attach(inst, host);
    const obs = new ResizeObserver(() => refit(inst));
    obs.observe(host);
    return () => {
      obs.disconnect();
      detach(inst); // keep the shell alive — never termClose here
    };
  }, [inst]);
  return <div ref={ref} className={`min-h-0 px-3 py-2 ${className ?? ""}`} />;
}

export function BottomTerminalDrawer({ sessionPk, projectName }: { sessionPk: string; projectName: string }) {
  const nav = useNav();
  const tabs = useTerms((s) => s.tabs[sessionPk] ?? []);
  const activeId = useTerms((s) => s.active[sessionPk]);
  const { open, ensureOne, close, setActive } = useTerms();
  const [query, setQuery] = useState("");
  const copyOnSelect = useTerms((s) => s.copyOnSelect);
  const setCopyOnSelect = useTerms((s) => s.setCopyOnSelect);

  // Spawn Terminal 1 only on mount / session change — not whenever the tab list
  // empties. ensureOne self-guards on existing tabs + an in-flight open, so this
  // is StrictMode-safe. Closing the last tab therefore leaves the drawer empty
  // (see the empty state below) instead of instantly respawning a terminal.
  useEffect(() => {
    void ensureOne(sessionPk);
  }, [sessionPk, ensureOne]);

  const active = tabs.find((t) => t.termId === activeId) ?? tabs[0];
  const inst = active ? getTerm(active.termId) : undefined;

  return (
    <div className="acrylic-panel flex shrink-0 flex-col border-t border-border" style={{ height: nav.bottomHeight }}>
      <PanelResizeHandle
        direction="y"
        onDelta={(d) => nav.setBottomHeight(clampPanelSize(nav.bottomHeight - d, window.innerHeight, BOTTOM_HEIGHT))}
      />
      <div className="flex shrink-0 items-center gap-2 border-b border-border px-3.5 py-1.5">
        <SquareTerminal aria-hidden size={14} strokeWidth={2} className="text-muted-foreground" />
        <div className="flex items-center gap-1 overflow-x-auto">
          {tabs.map((t) => (
            <div
              key={t.termId}
              className={`flex h-7 shrink-0 items-center gap-1 rounded-md border pl-2.5 pr-1 font-sans text-[12px] ${
                t.termId === active?.termId
                  ? "border-border bg-background text-foreground"
                  : "border-transparent text-muted-foreground hover:bg-accent"
              }`}
            >
              <button
                type="button"
                onClick={() => setActive(sessionPk, t.termId)}
                className={`cursor-pointer border-none bg-transparent font-sans text-inherit ${t.exited ? "line-through opacity-60" : ""}`}
              >
                {t.title}
              </button>
              <button type="button" title={`Close ${t.title}`} onClick={() => close(sessionPk, t.termId)} className={`${toolBtn} h-5 w-5`}>
                <X aria-hidden size={10} strokeWidth={2} />
              </button>
            </div>
          ))}
        </div>
        <button type="button" title="New terminal" onClick={() => void open(sessionPk)} className={`${toolBtn} h-7 w-7`}>
          <Plus aria-hidden size={13} strokeWidth={2} />
        </button>
        <form
          className="flex h-7 w-[180px] items-center gap-1.5 rounded-md border border-border px-2 [background:color-mix(in_oklab,var(--background)_45%,transparent)]"
          onSubmit={(e) => {
            e.preventDefault();
            if (inst && query) inst.search.findNext(query);
          }}
        >
          <Search aria-hidden size={11} strokeWidth={2} className="shrink-0 text-muted-foreground" />
          <input
            value={query}
            onChange={(e) => setQuery(e.target.value)}
            onKeyDown={(e) => {
              if (e.key === "Enter" && e.shiftKey && inst && query) {
                e.preventDefault();
                inst.search.findPrevious(query);
              }
            }}
            placeholder="Search"
            aria-label="Search terminal scrollback"
            className="min-w-0 flex-1 border-none bg-transparent font-sans text-[11.5px] text-foreground outline-none"
          />
        </form>
        <button
          type="button"
          title={copyOnSelect ? "Copy on select: on" : "Copy on select: off"}
          aria-pressed={copyOnSelect}
          onClick={() => setCopyOnSelect(!copyOnSelect)}
          className={`${toolBtn} h-7 w-7 ${copyOnSelect ? "bg-accent text-accent-foreground" : ""}`}
        >
          <Copy aria-hidden size={12} strokeWidth={2} />
        </button>
        <span className="font-mono text-[11px] text-muted-foreground">{projectName}</span>
        <div className="flex-1" />
        <button type="button" title="Close panel" onClick={nav.toggleBottom} className={`${toolBtn} h-[26px] w-[26px]`}>
          <X aria-hidden size={13} strokeWidth={2} />
        </button>
      </div>
      {inst ? (
        <TerminalHost inst={inst} className="flex-1" />
      ) : (
        <div className="flex flex-1 items-center justify-center font-sans text-[12.5px] text-muted-foreground">
          No terminal — press + to open one.
        </div>
      )}
    </div>
  );
}
