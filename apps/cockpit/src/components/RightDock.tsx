// apps/cockpit/src/components/RightDock.tsx
import { useState, useRef } from "react";
import { useUi } from "@/store-ui";
import { FileViewer } from "./FileViewer";
import { fileBadge } from "@/lib/paths";
import { Tabs, TabsList, TabsTab, Menu, MenuTrigger, MenuContent, MenuItem, MenuSeparator, Input } from "@ryuzi/ui";

const SOON = [
  { key: "terminal", label: "Terminal" },
  { key: "browser", label: "Browser" },
  { key: "sidechat", label: "Side chat" },
];

export function RightDock() {
  const { tabs, activeTabId, openFile, closeTab, setActiveTab } = useUi();
  const [path, setPath] = useState("");
  const pathInputRef = useRef<HTMLInputElement>(null);
  const active = tabs.find((t) => t.id === activeTabId) ?? null;

  const open = () => {
    const p = path.trim();
    if (p) {
      openFile(p);
      setPath("");
    }
  };

  return (
    <div className="flex h-full flex-col">
      {/* tab bar */}
      <div className="flex h-[42px] shrink-0 items-center gap-1 border-b border-border px-1.5">
        <Tabs value={activeTabId ?? ""} onValueChange={(v) => setActiveTab(String(v))} className="min-w-0 flex-1">
          <TabsList className="overflow-x-auto">
            {tabs.map((t) => (
              <TabsTab key={t.id} value={t.id}>
                <span className="rounded-[3px] bg-muted px-1 py-px text-[8.5px] font-bold text-muted-foreground">{fileBadge(t.path)}</span>
                <span className="truncate">{t.title}</span>
                {/* biome-ignore lint/a11y/useSemanticElements: must stay a <span> — it lives inside the Tab <button>, and a nested <button> is invalid HTML */}
                <span
                  role="button"
                  tabIndex={0}
                  aria-label={`Close ${t.title}`}
                  className="ml-0.5 flex opacity-50 hover:opacity-100"
                  onClick={(e) => {
                    e.stopPropagation();
                    closeTab(t.id);
                  }}
                  onKeyDown={(e) => {
                    if (e.key === "Enter" || e.key === " ") {
                      e.preventDefault();
                      e.stopPropagation();
                      closeTab(t.id);
                    }
                  }}
                >
                  <svg
                    aria-hidden="true"
                    width="11"
                    height="11"
                    viewBox="0 0 12 12"
                    fill="none"
                    stroke="currentColor"
                    strokeWidth="1.4"
                    strokeLinecap="round"
                  >
                    <path d="M3 3l6 6M9 3l-6 6" />
                  </svg>
                </span>
              </TabsTab>
            ))}
          </TabsList>
        </Tabs>
        <Menu>
          <MenuTrigger
            aria-label="New tab"
            className="flex h-7 w-7 items-center justify-center rounded-lg text-muted-foreground hover:bg-accent hover:text-foreground"
          >
            <svg
              aria-hidden="true"
              width="15"
              height="15"
              viewBox="0 0 24 24"
              fill="none"
              stroke="currentColor"
              strokeWidth="2"
              strokeLinecap="round"
            >
              <path d="M12 5v14M5 12h14" />
            </svg>
          </MenuTrigger>
          <MenuContent align="end" className="min-w-[200px]">
            <MenuItem onClick={() => pathInputRef.current?.focus()}>Files</MenuItem>
            <MenuSeparator />
            {SOON.map((s) => (
              <MenuItem key={s.key} disabled className="justify-between">
                {s.label}
                <span className="rounded-full bg-muted px-1.5 py-px text-[9px] font-bold tracking-wide text-muted-foreground uppercase">
                  soon
                </span>
              </MenuItem>
            ))}
          </MenuContent>
        </Menu>
      </div>

      {/* open-by-path input */}
      <div className="shrink-0 border-b border-border p-2">
        <Input
          ref={pathInputRef}
          value={path}
          onChange={(e) => setPath(e.target.value)}
          onKeyDown={(e) => {
            if (e.key === "Enter") open();
          }}
          placeholder="Absolute file path → Enter"
          className="h-8 font-mono text-[11.5px]"
        />
      </div>

      {/* active tab content */}
      {active ? (
        <FileViewer key={active.id} path={active.path} />
      ) : (
        <div className="flex flex-1 items-center justify-center px-4 text-center text-sm text-muted-foreground">
          Open a file by absolute path above.
        </div>
      )}
    </div>
  );
}
