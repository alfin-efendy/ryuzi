import { useEffect } from "react";
import { useStore } from "./store";
import { useUi } from "./store-ui";
import { ProjectsTree } from "./components/ProjectsTree";
import { SessionTranscript } from "./components/SessionTranscript";
import { RightDock } from "./components/RightDock";
import { TitleBar } from "./components/shell/TitleBar";
import { useDisableContextMenu } from "./lib/contextMenu";
import { Badge, Toaster } from "@ryuzi/ui";

export default function App() {
  const init = useStore((s) => s.init);
  const pending = useStore((s) => s.pendingApprovals.length);
  const { leftPanelOpen, rightPanelOpen } = useUi();
  const cols = `${leftPanelOpen ? "260px" : "0px"} 1fr ${rightPanelOpen ? "360px" : "0px"}`;
  useDisableContextMenu();
  useEffect(() => {
    init();
  }, [init]);
  return (
    <div className="flex h-screen flex-col overflow-hidden bg-surface-window text-foreground">
      <TitleBar />
      {pending > 0 && (
        <div className="flex shrink-0 items-center gap-2 border-b border-amber-500/30 bg-amber-500/10 px-4 py-1.5 text-xs text-amber-700 dark:text-amber-300">
          <Badge variant="secondary">{pending}</Badge> session(s) need approval
        </div>
      )}
      {/* Explicit grid-column placement keeps `main`/right pinned to their tracks even when a
          panel is display:none (auto-placement would otherwise reflow them). The minmax(0,1fr)
          row + min-h-0 chain constrains height so inner panes scroll instead of overflowing. */}
      <div className="grid min-h-0 flex-1 overflow-hidden" style={{ gridTemplateColumns: cols, gridTemplateRows: "minmax(0, 1fr)" }}>
        <aside style={{ gridColumn: 1 }} className={`min-h-0 overflow-hidden border-r border-border ${leftPanelOpen ? "" : "hidden"}`}>
          <ProjectsTree />
        </aside>
        <main style={{ gridColumn: 2 }} className="flex min-h-0 min-w-0 flex-col overflow-hidden">
          <SessionTranscript />
        </main>
        <aside style={{ gridColumn: 3 }} className={`min-h-0 overflow-hidden border-l border-border ${rightPanelOpen ? "" : "hidden"}`}>
          <RightDock />
        </aside>
      </div>
      <Toaster richColors position="bottom-right" />
    </div>
  );
}
