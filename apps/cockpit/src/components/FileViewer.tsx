import { useEffect, useState } from "react";
import CodeMirror from "@uiw/react-codemirror";
import { EditorView } from "@codemirror/view";
import type { LanguageSupport } from "@codemirror/language";
import { CircleAlert } from "lucide-react";
import { commands } from "@/bindings";
import { basename, toRepoRelative } from "@/lib/paths";
import { languageFor } from "@/lib/language";
import { cockpitCodeTheme } from "@/lib/codemirror-theme";
import { base64ToUtf8, previewImageSrc, previewKindForPath, type ViewMode } from "@/lib/preview";
import { Markdown } from "@/components/transcript/Markdown";

/** toRepoRelative leaves an absolute path unchanged when it doesn't start
 *  under `workdir` — that's how a tab opened via free-text absolute-path
 *  input pointing outside the session workdir is detected, without a second
 *  traversal check (the engine's jail is authoritative either way). */
function looksAbsolute(p: string): boolean {
  return p.startsWith("/") || /^[A-Za-z]:\//.test(p);
}

export function FileViewer({
  runnerId,
  sessionPk,
  path,
  mode,
  workdir,
}: {
  runnerId: string;
  sessionPk: string;
  path: string;
  mode: ViewMode;
  /** Session workdir, or null while it's still being fetched. */
  workdir: string | null;
}) {
  const [content, setContent] = useState("");
  const [imageSrc, setImageSrc] = useState<string | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [lang, setLang] = useState<LanguageSupport | null>(null);
  const kind = previewKindForPath(path);
  const rel = workdir !== null ? toRepoRelative(path, workdir) : null;
  const outsideWorkdir = rel !== null && looksAbsolute(rel);

  // One read per path serves both modes — toggling View|Code must not hit disk.
  // Images/svg go through readFileBase64 (readFile rejects non-UTF-8 bytes);
  // svg Code mode decodes the same payload instead of reading again. Reads
  // are jailed to the session workdir on the engine side (fsview::read_file*)
  // so this works over a remote pinned-TLS runner, not just locally.
  useEffect(() => {
    let cancelled = false;
    setContent("");
    setImageSrc(null);
    setError(null);
    setLang(null);
    if (rel === null) return; // workdir not resolved yet
    if (outsideWorkdir) {
      setError("File is outside the session workdir.");
      return;
    }
    if (kind === "image" || kind === "svg") {
      void commands.readFileBase64(runnerId, sessionPk, rel).then((res) => {
        if (cancelled) return;
        if (res.status === "ok") {
          setImageSrc(previewImageSrc(kind, res.data.contentType, res.data.dataBase64));
          if (kind === "svg") setContent(base64ToUtf8(res.data.dataBase64));
        } else setError(res.error.message);
      });
    } else {
      void commands.readFile(runnerId, sessionPk, rel).then((res) => {
        if (cancelled) return;
        if (res.status === "ok") setContent(res.data);
        else setError(res.error.message);
      });
    }
    // Language packs load lazily; failures just leave plain text.
    void languageFor(basename(path))
      ?.load()
      .then((support) => {
        if (!cancelled) setLang(support);
      })
      .catch(() => {});
    return () => {
      cancelled = true;
    };
  }, [runnerId, sessionPk, rel, outsideWorkdir, kind, path]);

  if (error) {
    return (
      <div className="flex flex-1 flex-col items-center justify-center gap-2 p-6 text-center">
        <CircleAlert aria-hidden size={18} strokeWidth={2} className="text-destructive" />
        <div className="font-sans text-[12.5px] text-destructive">{error}</div>
      </div>
    );
  }
  if (mode === "view" && kind === "markdown") {
    return (
      <div className="min-h-0 min-w-0 flex-1 overflow-auto p-4 font-sans text-[13px]">
        <Markdown text={content} />
      </div>
    );
  }
  if (mode === "view" && (kind === "image" || kind === "svg")) {
    return (
      <div className="flex min-h-0 min-w-0 flex-1 items-center justify-center overflow-auto p-4">
        {imageSrc && <img src={imageSrc} alt={basename(path)} className="max-h-full max-w-full object-contain" />}
      </div>
    );
  }
  if (mode === "view" && kind === "html") {
    // sandbox="" applies every restriction: no scripts, no forms, no popups.
    return <iframe title={basename(path)} sandbox="" srcDoc={content} className="min-h-0 w-full flex-1 border-0 bg-white" />;
  }
  if (kind === "image") {
    return (
      <div className="flex flex-1 items-center justify-center font-sans text-[12.5px] text-muted-foreground">
        Binary image — no text view.
      </div>
    );
  }
  return (
    <div className="min-h-0 min-w-0 flex-1 overflow-auto">
      <CodeMirror
        value={content}
        editable={false}
        theme={cockpitCodeTheme}
        extensions={lang ? [EditorView.lineWrapping, lang] : [EditorView.lineWrapping]}
      />
    </div>
  );
}
