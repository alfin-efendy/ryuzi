// apps/cockpit/src/components/shell/TitleBar.tsx
import { useUi } from "@/store-ui";
import { WindowControls } from "./WindowControls";

const tool = "flex h-[30px] w-8 items-center justify-center rounded-md text-muted-foreground transition-colors hover:bg-accent hover:text-foreground";
const on = "bg-primary/10 text-primary";

export function TitleBar() {
  const { leftPanelOpen, rightPanelOpen, toggleLeft, toggleRight } = useUi();
  return (
    <div className="flex h-11 shrink-0 select-none items-center border-b border-border bg-background pr-1.5 pl-3">
      <div className="flex items-center gap-2">
        <div className="flex h-[22px] w-[22px] items-center justify-center rounded-[7px] bg-primary text-primary-foreground">
          <svg width="13" height="13" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2.4" strokeLinecap="round"><path d="M12 3a9 9 0 1 0 9 9" /><path d="M12 3v9l6 3" /></svg>
        </div>
        <span className="text-[13px] font-semibold tracking-tight">Cockpit</span>
      </div>
      <div data-tauri-drag-region className="h-full flex-1" />
      <div className="mr-1.5 flex items-center gap-0.5">
        <button type="button" aria-label="Toggle left panel" className={`${tool} ${leftPanelOpen ? on : ""}`} onClick={toggleLeft}>
          <svg width="16" height="16" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="1.8"><rect x="3" y="4" width="18" height="16" rx="2" /><path d="M9 4v16" strokeWidth="2.2" /></svg>
        </button>
        <button type="button" aria-label="Toggle right panel" className={`${tool} ${rightPanelOpen ? on : ""}`} onClick={toggleRight}>
          <svg width="16" height="16" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="1.8"><rect x="3" y="4" width="18" height="16" rx="2" /><path d="M15 4v16" strokeWidth="2.2" /></svg>
        </button>
      </div>
      <div className="mr-1 h-[18px] w-px bg-border" />
      <WindowControls />
    </div>
  );
}
