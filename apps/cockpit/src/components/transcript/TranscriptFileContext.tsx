import { createContext, useContext, useEffect, useState } from "react";
import { Button, cn } from "@ryuzi/ui";
import { joinPath, looksLikeWorkspaceFilePath, parsePathToken, toWorkspaceRelativePath } from "@/lib/paths";
import { workspaceFileExists } from "@/lib/file-probe";
import { useUi } from "@/store-ui";
import { useNav } from "@/store-nav";

export type TranscriptFileCtx = { runnerId: string; sessionPk: string; workdir: string };

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
 *  <code> until the (cached) existence probe confirms; then a design-system
 *  `Button` re-skinned to look like inline code — real button semantics give
 *  free keyboard (Enter/Space) activation and satisfy a11y linting, unlike a
 *  role="button" <code> span. */
export function WorkspacePathCode({ text, className }: { text: string; className?: string }) {
  const ctx = useContext(TranscriptFileContext);
  const open = useOpenWorkspaceFile();
  const [exists, setExists] = useState(false);

  // `src/a.ts:42[:5]` opens the file: parsePathToken strips the line/col
  // suffix when the token has one; everything else passes through raw. (Line
  // jumping itself is not supported by the viewer yet — the path still opens.)
  const pathText = parsePathToken(text)?.path ?? text;
  const rel = ctx ? toWorkspaceRelativePath(pathText, ctx.workdir) : null;
  // Trusted = the span was an absolute path that resolved under the workdir;
  // only those may skip the shape heuristic (they still get probed).
  const inputWasAbsolute = pathText.startsWith("/") || /^[A-Za-z]:[\\/]/.test(pathText);
  const trusted = rel !== null && inputWasAbsolute;
  // The shape heuristic sees a separator-normalized form so Windows-style
  // relative paths (`crates\core\lib.rs`) qualify; the existence probe stays
  // the gate that decides whether anything actually links.
  const shapeText = pathText.replace(/\\/g, "/");
  const candidate = ctx && rel !== null && (trusted || looksLikeWorkspaceFilePath(shapeText)) ? rel : null;

  useEffect(() => {
    setExists(false);
    if (!ctx || candidate === null) return;
    let alive = true;
    void workspaceFileExists(ctx.runnerId, ctx.sessionPk, candidate).then((ok) => {
      if (alive) setExists(ok);
    });
    return () => {
      alive = false;
    };
  }, [ctx, candidate]);

  if (!ctx || candidate === null || !exists || open === null) return <code className={className}>{text}</code>;
  return (
    <Button
      variant="ghost"
      size="xs"
      className={cn(
        "h-auto cursor-pointer rounded-[4px] border border-border bg-code px-[0.35em] py-[0.1em] font-mono text-[12px] text-code-foreground underline decoration-dotted underline-offset-2",
        className,
      )}
      onClick={() => open(candidate)}
    >
      {text}
    </Button>
  );
}
