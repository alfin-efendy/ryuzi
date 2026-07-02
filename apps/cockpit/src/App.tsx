import { useEffect, useRef } from "react";
import { useStore } from "./store";
import { useUi } from "./store-ui";
import { ProjectsTree } from "./components/ProjectsTree";
import { SessionTranscript } from "./components/SessionTranscript";
import { RightDock } from "./components/RightDock";
import { TitleBar } from "./components/shell/TitleBar";
import { useDisableContextMenu } from "./lib/contextMenu";
import {
  Badge,
  ResizableGroup,
  ResizablePanel,
  ResizableSeparator,
  Toaster,
  useDefaultLayout,
  type PanelImperativeHandle,
} from "@ryuzi/ui";

/** Animate programmatic collapse/expand only — drag must stay direct. */
function useToggleSync(
  open: boolean,
  panel: React.RefObject<PanelImperativeHandle | null>,
  group: React.RefObject<HTMLDivElement | null>,
) {
  useEffect(() => {
    const p = panel.current;
    if (!p) return;
    if (open === !p.isCollapsed()) return;
    const g = group.current;
    g?.classList.add("panels-animating");
    if (open) p.expand();
    else p.collapse();
    const t = setTimeout(() => g?.classList.remove("panels-animating"), 250);
    return () => clearTimeout(t);
  }, [open, panel, group]);
}

export default function App() {
  const init = useStore((s) => s.init);
  const pending = useStore((s) => s.pendingApprovals.length);
  const { leftPanelOpen, rightPanelOpen, setLeft, setRight } = useUi();
  const leftPanel = useRef<PanelImperativeHandle>(null);
  const rightPanel = useRef<PanelImperativeHandle>(null);
  const groupEl = useRef<HTMLDivElement>(null);
  // Panels are always mounted (never conditionally rendered), so panelIds is
  // unnecessary here — it only matters for conditionally-rendered panels per
  // the useDefaultLayout .d.ts JSDoc.
  const { defaultLayout, onLayoutChanged } = useDefaultLayout({
    id: "cockpit-main",
    storage: typeof localStorage === "undefined" ? undefined : localStorage,
  });
  useDisableContextMenu();
  useEffect(() => {
    init();
  }, [init]);
  useToggleSync(leftPanelOpen, leftPanel, groupEl);
  useToggleSync(rightPanelOpen, rightPanel, groupEl);
  return (
    <div className="flex h-screen flex-col overflow-hidden bg-surface-window text-foreground">
      <TitleBar />
      {pending > 0 && (
        <div className="flex shrink-0 items-center gap-2 border-b border-amber-500/30 bg-amber-500/10 px-4 py-1.5 text-xs text-amber-700 dark:text-amber-300">
          <Badge variant="secondary">{pending}</Badge> session(s) need approval
        </div>
      )}
      <ResizableGroup
        elementRef={groupEl}
        orientation="horizontal"
        className="min-h-0 flex-1"
        defaultLayout={defaultLayout}
        onLayoutChanged={onLayoutChanged}
      >
        <ResizablePanel
          panelRef={leftPanel}
          id="left"
          collapsible
          defaultSize="260px"
          minSize="200px"
          maxSize="400px"
          className="min-h-0 overflow-hidden"
          onResize={() => {
            const p = leftPanel.current;
            if (p) setLeft(!p.isCollapsed());
          }}
        >
          <ProjectsTree />
        </ResizablePanel>
        <ResizableSeparator />
        <ResizablePanel id="center" minSize="360px" className="flex min-h-0 min-w-0 flex-col overflow-hidden bg-surface-layer">
          <SessionTranscript />
        </ResizablePanel>
        <ResizableSeparator />
        <ResizablePanel
          panelRef={rightPanel}
          id="right"
          collapsible
          defaultSize="360px"
          minSize="280px"
          maxSize="560px"
          className="min-h-0 overflow-hidden bg-surface-layer"
          onResize={() => {
            const p = rightPanel.current;
            if (p) setRight(!p.isCollapsed());
          }}
        >
          <RightDock />
        </ResizablePanel>
      </ResizableGroup>
      <Toaster richColors position="bottom-right" />
    </div>
  );
}
