// apps/ide/src/renderer/app.tsx
import React, { useEffect } from "react";
import { hydrate } from "./ipc-bridge";
import { TopBar } from "./screens/TopBar";
import { ProjectsSessionsTree } from "./screens/ProjectsSessionsTree";
import { SessionTranscript } from "./screens/SessionTranscript";
import { ApprovalsRail } from "./screens/ApprovalsRail";
import { ConnectProjectDialog } from "./screens/ConnectProjectDialog";

export function App() {
  useEffect(() => hydrate(), []);
  return (
    <div className="flex h-screen flex-col bg-background text-foreground">
      <TopBar />
      <div className="flex min-h-0 flex-1">
        <aside className="flex w-72 shrink-0 flex-col overflow-hidden border-r">
          <div className="flex items-center justify-between border-b p-2">
            <span className="text-xs uppercase tracking-wide text-muted-foreground">Projects</span>
            <ConnectProjectDialog />
          </div>
          <div className="min-h-0 flex-1 overflow-auto">
            <ProjectsSessionsTree />
          </div>
        </aside>
        <main className="min-w-0 flex-1 overflow-auto">
          <SessionTranscript />
        </main>
        <aside className="w-72 shrink-0 overflow-auto border-l" data-testid="right-rail">
          <ApprovalsRail />
        </aside>
      </div>
    </div>
  );
}
