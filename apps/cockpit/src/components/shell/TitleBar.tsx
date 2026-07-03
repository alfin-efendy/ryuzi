// apps/cockpit/src/components/shell/TitleBar.tsx
import { useEffect, useRef } from "react";
import { ArrowLeft, ArrowRight, PanelLeft, Search } from "lucide-react";
import { useNav } from "@/store-nav";
import { WindowControls } from "./WindowControls";

const isMac = typeof navigator !== "undefined" && /Mac/i.test(navigator.userAgent);

const tool =
  "flex h-[30px] w-[30px] cursor-pointer items-center justify-center rounded-md border-none bg-transparent text-muted-foreground hover:bg-accent hover:text-accent-foreground";

export function TitleBar() {
  const history = useNav((s) => s.history);
  const { goBack, goForward, toggleSidebar, searchQuery, setSearchQuery } = useNav();
  const searchRef = useRef<HTMLInputElement>(null);

  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if ((e.metaKey || e.ctrlKey) && e.key.toLowerCase() === "k") {
        e.preventDefault();
        searchRef.current?.focus();
      }
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, []);

  const canBack = history.back.length > 0;
  const canForward = history.forward.length > 0;

  return (
    <div
      data-tauri-drag-region="deep"
      className={`acrylic-chrome relative z-20 flex h-11 shrink-0 select-none items-center gap-2.5 border-b border-border ${isMac ? "pl-[78px]" : "pl-3.5"} pr-3.5`}
    >
      <div className="flex items-center gap-0.5">
        <button type="button" title="Toggle sidebar" aria-label="Toggle sidebar" className={tool} onClick={toggleSidebar}>
          <PanelLeft aria-hidden size={15} strokeWidth={2} />
        </button>
        <button
          type="button"
          title="Back"
          aria-label="Back"
          className={`${tool} ${canBack ? "text-foreground" : "cursor-default opacity-50 hover:bg-transparent"}`}
          onClick={goBack}
          disabled={!canBack}
        >
          <ArrowLeft aria-hidden size={15} strokeWidth={2} />
        </button>
        <button
          type="button"
          title="Forward"
          aria-label="Forward"
          className={`${tool} ${canForward ? "text-foreground" : "cursor-default opacity-50 hover:bg-transparent"}`}
          onClick={goForward}
          disabled={!canForward}
        >
          <ArrowRight aria-hidden size={15} strokeWidth={2} />
        </button>
      </div>

      <div className="flex flex-1 justify-center">
        <div className="flex h-[30px] w-full max-w-[420px] items-center gap-2 rounded-md border border-border px-3 text-muted-foreground [background:color-mix(in_oklab,var(--background)_45%,transparent)]">
          <Search aria-hidden size={13} strokeWidth={2} />
          <input
            ref={searchRef}
            value={searchQuery}
            onChange={(e) => setSearchQuery(e.target.value)}
            placeholder="Search sessions, files, commands"
            className="flex-1 border-none bg-transparent font-sans text-[12.5px] text-foreground"
          />
          <kbd className="rounded-sm border border-border px-[5px] py-px font-mono text-[10.5px] text-muted-foreground">
            {isMac ? "⌘K" : "Ctrl K"}
          </kbd>
        </div>
      </div>

      <div className="flex w-[92px] shrink-0 items-center justify-end">{!isMac && <WindowControls />}</div>
    </div>
  );
}
