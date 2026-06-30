import { useEffect, useState } from "react";
import CodeMirror from "@uiw/react-codemirror";
import { EditorView } from "@codemirror/view";
import { commands } from "@/bindings";

export function FileViewer({ path }: { path: string }) {
  const [content, setContent] = useState("");
  useEffect(() => {
    let cancelled = false;
    commands.readFile(path).then((res) => {
      if (cancelled) return;
      setContent(res.status === "ok" ? res.data : `Error: ${res.error.message}`);
    });
    return () => { cancelled = true; };
  }, [path]);
  return (
    <div className="flex-1 overflow-auto">
      <CodeMirror value={content} editable={false} extensions={[EditorView.lineWrapping]} />
    </div>
  );
}
