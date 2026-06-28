// apps/ide/src/renderer/screens/ProjectsSessionsTree.tsx
import React from "react";
import { useStore } from "../store";
import { NewSessionDialog } from "./NewSessionDialog";

export function ProjectsSessionsTree() {
  const projects = useStore((s) => s.projects);
  const sessions = useStore((s) => s.sessions);
  const activeSessionPk = useStore((s) => s.activeSessionPk);
  const setActive = useStore((s) => s.setActive);
  return (
    <div className="p-2 text-sm" data-testid="tree">
      {projects.map((p) => (
        <div key={p.projectId} className="mb-2">
          <div className="flex items-center justify-between">
            <span className="font-medium">{p.name}</span>
            <NewSessionDialog projectId={p.projectId} />
          </div>
          {sessions
            .filter((s) => s.projectId === p.projectId)
            .map((s) => (
              <button
                type="button"
                key={s.sessionPk}
                onClick={() => setActive(s.sessionPk)}
                className={`flex w-full items-center gap-2 rounded px-2 py-1 text-left ${activeSessionPk === s.sessionPk ? "bg-accent" : ""}`}
              >
                <span className={`inline-block h-2 w-2 rounded-full ${s.status === "running" ? "bg-green-500" : "bg-gray-500"}`} />
                <span className="truncate">{s.title ?? s.sessionPk.slice(0, 8)}</span>
              </button>
            ))}
        </div>
      ))}
    </div>
  );
}
