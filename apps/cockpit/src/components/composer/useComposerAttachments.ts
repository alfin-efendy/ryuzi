import { useCallback, useEffect, useState } from "react";
import { getCurrentWebview } from "@tauri-apps/api/webview";
import { toast } from "sonner";
import { commands } from "@/bindings";
import { fileToBase64 } from "@/lib/attachments";

/** Composer attachment state shared by HomeView and SessionView: native
 *  picker, clipboard paste (files/images), and webview drag-drop. */
export function useComposerAttachments() {
  const [attachments, setAttachments] = useState<string[]>([]);
  const [dragOver, setDragOver] = useState(false);

  const add = useCallback((paths: string[]) => {
    if (paths.length) setAttachments((cur) => Array.from(new Set([...cur, ...paths])));
  }, []);
  const remove = useCallback((path: string) => setAttachments((cur) => cur.filter((p) => p !== path)), []);
  const clear = useCallback(() => setAttachments([]), []);

  const attachFiles = useCallback(async () => {
    add(await commands.pickFiles());
  }, [add]);

  /** Paste handler for the composer textarea — stages clipboard files/images
   *  to disk via the backend, then treats them like picked paths. */
  const onPaste = useCallback(
    (e: React.ClipboardEvent) => {
      const files = Array.from(e.clipboardData?.files ?? []);
      if (files.length === 0) return;
      e.preventDefault();
      void (async () => {
        for (const file of files) {
          const name = file.name || (file.type.startsWith("image/") ? `pasted.${file.type.slice(6) || "png"}` : "pasted.bin");
          const res = await commands.stageAttachment(name, await fileToBase64(file));
          if (res.status === "ok") add([res.data]);
          else toast.error("Couldn't attach pasted file: " + res.error.message);
        }
      })();
    },
    [add],
  );

  // Native drag-drop delivers real file paths (Tauri webview event).
  useEffect(() => {
    let unlisten: (() => void) | undefined;
    let cancelled = false;
    void getCurrentWebview()
      .onDragDropEvent((event) => {
        if (event.payload.type === "drop") {
          setDragOver(false);
          add(event.payload.paths);
        } else if (event.payload.type === "enter" || event.payload.type === "over") {
          setDragOver(true);
        } else {
          setDragOver(false);
        }
      })
      .then((fn) => {
        if (cancelled) fn();
        else unlisten = fn;
      });
    return () => {
      cancelled = true;
      unlisten?.();
    };
  }, [add]);

  return { attachments, add, remove, clear, attachFiles, onPaste, dragOver };
}
