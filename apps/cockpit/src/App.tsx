import { useEffect } from "react";
import { useStore } from "./store";
import { ProjectsTree } from "./components/ProjectsTree";
import { SessionTranscript } from "./components/SessionTranscript";
import { FileViewer } from "./components/FileViewer";
import { Badge, Toaster } from "@harness/ui";

export default function App() {
  const init = useStore((s) => s.init);
  const pending = useStore((s) => s.pendingApprovals.length);
  useEffect(() => {
    init();
  }, [init]);
  return (
    <div className="flex h-screen flex-col bg-white text-zinc-900 dark:bg-zinc-950 dark:text-zinc-100">
      {pending > 0 && (
        <div className="bg-amber-100 px-4 py-1 text-xs dark:bg-amber-950/40">
          <Badge variant="secondary">{pending}</Badge> session(s) need approval
        </div>
      )}
      <div className="grid flex-1 grid-cols-[260px_1fr_360px]">
        <aside className="border-r border-zinc-200 dark:border-zinc-800">
          <ProjectsTree />
        </aside>
        <main className="min-w-0">
          <SessionTranscript />
        </main>
        <aside className="border-l border-zinc-200 dark:border-zinc-800">
          <FileViewer />
        </aside>
      </div>
      <Toaster richColors position="bottom-right" />
    </div>
  );
}
