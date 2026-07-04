import { useEffect, useState } from "react";
import CodeMirror from "@uiw/react-codemirror";
import { EditorView } from "@codemirror/view";
import type { LanguageSupport } from "@codemirror/language";
import { CircleAlert } from "lucide-react";
import { commands } from "@/bindings";
import { basename } from "@/lib/paths";
import { languageFor } from "@/lib/language";

export function FileViewer({ path }: { path: string }) {
  const [content, setContent] = useState("");
  const [error, setError] = useState<string | null>(null);
  const [lang, setLang] = useState<LanguageSupport | null>(null);

  useEffect(() => {
    let cancelled = false;
    setError(null);
    setLang(null);
    commands.readFile(path).then((res) => {
      if (cancelled) return;
      if (res.status === "ok") setContent(res.data);
      else {
        setContent("");
        setError(res.error.message);
      }
    });
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
  }, [path]);

  if (error) {
    return (
      <div className="flex flex-1 flex-col items-center justify-center gap-2 p-6 text-center">
        <CircleAlert aria-hidden size={18} strokeWidth={2} className="text-destructive" />
        <div className="font-sans text-[12.5px] text-destructive">{error}</div>
      </div>
    );
  }
  return (
    <div className="flex-1 overflow-auto">
      <CodeMirror value={content} editable={false} extensions={lang ? [EditorView.lineWrapping, lang] : [EditorView.lineWrapping]} />
    </div>
  );
}
