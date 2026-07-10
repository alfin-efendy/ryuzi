import { createContext, useContext, useEffect, useState } from "react";
import { cn } from "@ryuzi/ui";
import { joinPath, looksLikeWorkspaceFilePath, toWorkspaceRelativePath } from "@/lib/paths";
import { workspaceFileExists } from "@/lib/file-probe";
import { useUi } from "@/store-ui";
import { useNav } from "@/store-nav";

export type TranscriptFileCtx = { sessionPk: string; workdir: string };

/** Session file context for transcript markdown: present only under a
 *  session's Transcript, so shared Markdown consumers (FileViewer, …) render
 *  plain code spans unchanged. */
export const TranscriptFileContext = createContext<TranscriptFileCtx | null>(null);

/** Click handler that opens a workdir-relative path in the right-panel file
 *  viewer — the same flow as the file tree and the command palette. Returns
 *  null outside a provider. */
export function useOpenWorkspaceFile(): ((rel: string) => void) | null {
  const ctx = useContext(TranscriptFileContext);
  const openFile = useUi((s) => s.openFile);
  const setRightOpen = useNav((s) => s.setRightOpen);
  const setRightTab = useNav((s) => s.setRightTab);
  if (!ctx) return null;
  return (rel) => {
    openFile(joinPath(ctx.workdir, rel));
    setRightOpen(true);
    setRightTab("file");
  };
}

/** Inline code span that linkifies real workspace file paths. Renders a plain
 *  <code> until the (cached) existence probe confirms; then a native <button>
 *  styled like inline code — real button semantics give free keyboard
 *  (Enter/Space) activation and satisfy a11y linting, unlike a role="button"
 *  <code> span. */
export function WorkspacePathCode({ text, className }: { text: string; className?: string }) {
  const ctx = useContext(TranscriptFileContext);
  const open = useOpenWorkspaceFile();
  const [exists, setExists] = useState(false);

  const rel = ctx ? toWorkspaceRelativePath(text, ctx.workdir) : null;
  const trusted = rel !== null && rel !== text; // an absolute form resolved under the workdir
  const candidate = ctx && rel !== null && (trusted || looksLikeWorkspaceFilePath(text)) ? rel : null;

  useEffect(() => {
    if (!ctx || candidate === null) return;
    let alive = true;
    void workspaceFileExists(ctx.sessionPk, candidate).then((ok) => {
      if (alive) setExists(ok);
    });
    return () => {
      alive = false;
    };
  }, [ctx, candidate]);

  if (!ctx || candidate === null || !exists || open === null) return <code className={className}>{text}</code>;
  return (
    <button
      type="button"
      className={cn(
        "cursor-pointer rounded-[4px] border border-border bg-code px-[0.35em] py-[0.1em] font-mono text-[12px] text-code-foreground underline decoration-dotted underline-offset-2",
        className,
      )}
      onClick={() => open(candidate)}
    >
      {text}
    </button>
  );
}
