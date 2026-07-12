import { useEffect, useState } from "react";
import { AudioLines, Film, Paperclip, X } from "lucide-react";
import { Button } from "@ryuzi/ui";
import { commands } from "@/bindings";
import { basename } from "@/lib/paths";
import { mediaKindForPath } from "@/lib/attachments";

/** Image thumbnail via read_local_media — these paths are CLIENT-LOCAL files
 *  the user picked/dropped to attach (staged via stage_attachment on send),
 *  which arbitrary-path local reads correctly, even for a remote session.
 *  Falls back to a plain chip on any error. */
function ImageChip({ path, onRemove }: { path: string; onRemove: () => void }) {
  const [src, setSrc] = useState<string | null>(null);
  const [failed, setFailed] = useState(false);
  useEffect(() => {
    let cancelled = false;
    void commands.readLocalMedia(path).then((res) => {
      if (cancelled) return;
      if (res.status === "ok") setSrc(`data:${res.data.contentType ?? "image/png"};base64,${res.data.dataBase64}`);
      else setFailed(true);
    });
    return () => {
      cancelled = true;
    };
  }, [path]);
  if (failed || src === null) return <FileChip path={path} icon={Paperclip} onRemove={onRemove} />;
  return (
    <div className="relative">
      <img src={src} alt={basename(path)} title={path} className="h-12 w-12 rounded-lg border border-border object-cover" />
      <Button
        variant="secondary"
        size="icon-xs"
        title={`Remove ${basename(path)}`}
        onClick={onRemove}
        className="absolute -right-1.5 -top-1.5 size-4 rounded-full"
      >
        <X aria-hidden size={9} strokeWidth={2.5} />
      </Button>
    </div>
  );
}

function FileChip({ path, icon: Icon, onRemove }: { path: string; icon: typeof Paperclip; onRemove: () => void }) {
  return (
    <Button
      variant="outline"
      size="sm"
      title={path}
      onClick={onRemove}
      className="max-w-[220px] rounded-full px-2 text-[12px] text-muted-foreground"
    >
      <Icon aria-hidden size={12} strokeWidth={2} className="size-3 shrink-0" />
      <span className="truncate">{basename(path)}</span>
      <X aria-hidden size={11} strokeWidth={2} className="size-[11px] shrink-0" />
    </Button>
  );
}

export function AttachmentChips({ attachments, onRemove }: { attachments: string[]; onRemove: (path: string) => void }) {
  return (
    <>
      {attachments.map((path) => {
        const kind = mediaKindForPath(path);
        if (kind === "image") return <ImageChip key={path} path={path} onRemove={() => onRemove(path)} />;
        const icon = kind === "video" ? Film : kind === "audio" ? AudioLines : Paperclip;
        return <FileChip key={path} path={path} icon={icon} onRemove={() => onRemove(path)} />;
      })}
    </>
  );
}
