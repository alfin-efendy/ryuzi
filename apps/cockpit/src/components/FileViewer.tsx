import { useState } from "react";
import CodeMirror from "@uiw/react-codemirror";
import { EditorView } from "@codemirror/view";
import { commands } from "@/bindings";
import { Input } from "@harness/ui";

export function FileViewer() {
  const [path, setPath] = useState("");
  const [content, setContent] = useState("");
  const open = async () => {
    const res = await commands.readFile(path);
    setContent(res.status === "ok" ? res.data : `Error: ${res.error.message}`);
  };
  return (
    <div className="flex h-full flex-col">
      <div className="border-b border-zinc-200 p-2 dark:border-zinc-800">
        <Input
          value={path}
          onChange={(e) => setPath(e.target.value)}
          onKeyDown={(e) => {
            if (e.key === "Enter") open();
          }}
          placeholder="Absolute file path → Enter"
          className="h-8"
        />
      </div>
      <div className="flex-1 overflow-auto">
        <CodeMirror value={content} editable={false} extensions={[EditorView.lineWrapping]} />
      </div>
    </div>
  );
}
