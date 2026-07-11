import { useEffect, useRef, useState } from "react";
import { Copy, Plus, Search, SquareTerminal, X } from "lucide-react";
import { useNav, clampPanelSize, BOTTOM_HEIGHT } from "@/store-nav";
import { useTerms } from "@/store-terms";
import { LOCAL_RUNNER, sessKey } from "@/lib/session-key";
import { attach, detach, getTerm, refit, type TermInstance } from "@/lib/term-cache";
import { Button, Input } from "@ryuzi/ui";
import { PanelResizeHandle } from "@/components/common/PanelResizeHandle";

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

export function BottomTerminalDrawer({ runnerId, sessionPk, projectName }: { runnerId: string; sessionPk: string; projectName: string }) {
  const nav = useNav();
  const key = sessKey(runnerId, sessionPk);
  const tabs = useTerms((s) => s.tabs[key] ?? []);
  const activeId = useTerms((s) => s.active[key]);
  const { open, ensureOne, close, setActive } = useTerms();
  const [query, setQuery] = useState("");
  const copyOnSelect = useTerms((s) => s.copyOnSelect);
  const setCopyOnSelect = useTerms((s) => s.setCopyOnSelect);

  // Spawn Terminal 1 only on mount / session change — not whenever the tab list
  // empties. ensureOne self-guards on existing tabs + an in-flight open, so this
  // is StrictMode-safe. Closing the last tab therefore leaves the drawer empty
  // (see the empty state below) instead of instantly respawning a terminal.
  // Belt-and-suspenders: SessionView's render guard is what actually keeps this
  // component from mounting for a remote session, but a local ConPTY/bash has
  // no meaning on a remote host either way, so refuse to spawn one here too.
  useEffect(() => {
    if (runnerId !== LOCAL_RUNNER) return;
    void ensureOne(runnerId, sessionPk);
  }, [runnerId, sessionPk, ensureOne]);

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
              <Button
                variant="ghost"
                size="xs"
                onClick={() => setActive(runnerId, sessionPk, t.termId)}
                className={`h-auto p-0 text-inherit hover:bg-transparent hover:text-inherit dark:hover:bg-transparent ${
                  t.exited ? "line-through opacity-60" : ""
                }`}
              >
                {t.title}
              </Button>
              <Button
                variant="ghost"
                size="icon-xs"
                title={`Close ${t.title}`}
                onClick={() => close(runnerId, sessionPk, t.termId)}
                className="size-5 text-muted-foreground"
              >
                <X aria-hidden size={10} strokeWidth={2} className="size-2.5" />
              </Button>
            </div>
          ))}
        </div>
        <Button
          variant="ghost"
          size="icon-sm"
          title="New terminal"
          onClick={() => void open(runnerId, sessionPk)}
          className="text-muted-foreground"
        >
          <Plus aria-hidden size={13} strokeWidth={2} className="size-[13px]" />
        </Button>
        <form
          className="flex h-7 w-[180px] items-center gap-1.5 rounded-md border border-border px-2 [background:color-mix(in_oklab,var(--background)_45%,transparent)]"
          onSubmit={(e) => {
            e.preventDefault();
            if (inst && query) inst.search.findNext(query);
          }}
        >
          <Search aria-hidden size={11} strokeWidth={2} className="shrink-0 text-muted-foreground" />
          <Input
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
            className="h-full flex-1 rounded-none border-none bg-transparent px-0 text-foreground focus-visible:ring-0 dark:bg-transparent"
          />
        </form>
        <Button
          variant="ghost"
          size="icon-sm"
          title={copyOnSelect ? "Copy on select: on" : "Copy on select: off"}
          aria-pressed={copyOnSelect}
          onClick={() => setCopyOnSelect(!copyOnSelect)}
          className={copyOnSelect ? "bg-accent text-accent-foreground" : "text-muted-foreground"}
        >
          <Copy aria-hidden size={12} strokeWidth={2} className="size-3" />
        </Button>
        <span className="font-mono text-[11px] text-muted-foreground">{projectName}</span>
        <div className="flex-1" />
        <Button variant="ghost" size="icon-sm" title="Close panel" onClick={nav.toggleBottom} className="size-[26px] text-muted-foreground">
          <X aria-hidden size={13} strokeWidth={2} className="size-[13px]" />
        </Button>
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
