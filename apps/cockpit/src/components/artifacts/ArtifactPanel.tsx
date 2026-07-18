import { useEffect, useState } from "react";
import { Download, Eye, FileText, Link2 } from "lucide-react";
import { Badge, Button, SettingsCard } from "@ryuzi/ui";
import { commands, type ArtifactFileInfo, type ArtifactInfo } from "@/bindings";
import { ArtifactPreview } from "./ArtifactPreview";

type Props = { runnerId: string; sessionPk: string; refreshKey?: unknown };

function bytes(value: number) {
  if (value < 1024) return `${value} B`;
  if (value < 1024 * 1024) return `${(value / 1024).toFixed(1)} KB`;
  return `${(value / (1024 * 1024)).toFixed(1)} MB`;
}

function label(status: string) {
  if (status === "deleted") return "Deleted after retention";
  if (status === "source-archived") return "Source archived";
  return "Available";
}

function save(file: ArtifactFileInfo) {
  const raw = Uint8Array.from(atob(file.dataBase64), (char) => char.charCodeAt(0));
  const url = URL.createObjectURL(new Blob([raw], { type: file.contentType ?? "application/octet-stream" }));
  const anchor = document.createElement("a");
  anchor.href = url;
  anchor.download = file.name;
  anchor.click();
  URL.revokeObjectURL(url);
}

export function ArtifactPanel({ runnerId, sessionPk, refreshKey }: Props) {
  const [items, setItems] = useState<ArtifactInfo[]>([]);
  const [error, setError] = useState<string | null>(null);
  const [preview, setPreview] = useState<ArtifactInfo | null>(null);

  const load = () =>
    void commands.listSessionArtifacts(runnerId, sessionPk).then((result) => {
      if (result.status === "ok") {
        setItems(result.data);
        setError(null);
      } else setError(result.error.message);
    });

  useEffect(load, [runnerId, sessionPk, refreshKey]);

  const download = (artifact: ArtifactInfo) =>
    void commands.fetchArtifact(runnerId, sessionPk, artifact.id).then((result) => {
      if (result.status === "ok") save(result.data);
      else setError(result.error.message);
    });

  return (
    <SettingsCard className="mx-4 mb-3 shrink-0 overflow-hidden">
      <div className="flex items-center justify-between px-3 py-2">
        <div className="flex items-center gap-2 text-sm font-semibold">
          <FileText aria-hidden size={14} />
          Artifacts
        </div>
        <Button variant="ghost" size="sm" onClick={load}>
          Refresh
        </Button>
      </div>
      {error ? <div className="px-3 pb-2 text-xs text-destructive">{error}</div> : null}
      {items.length === 0 ? <div className="px-3 pb-3 text-xs text-muted-foreground">No artifacts in this session.</div> : null}
      <div className="divide-y divide-border">
        {items.map((artifact) => {
          const unavailable = artifact.status === "deleted";
          return (
            <div key={`${artifact.id}:${artifact.referenceId ?? "source"}`} className="flex min-h-12 items-center gap-2 px-3 py-2 text-xs">
              <FileText aria-hidden size={14} className="shrink-0 text-muted-foreground" />
              <div className="min-w-0 flex-1">
                <div className="truncate font-medium">{artifact.name}</div>
                <div className="flex items-center gap-1 truncate text-muted-foreground">
                  {artifact.referenceId ? (
                    <>
                      <Link2 aria-hidden size={11} />
                      Shared from {artifact.sharedFromSessionPk}
                    </>
                  ) : (
                    <>
                      {artifact.creator} · {artifact.contentType ?? "file"} · {bytes(artifact.sizeBytes)}
                    </>
                  )}
                </div>
              </div>
              <Badge variant={unavailable ? "secondary" : "outline"}>{label(artifact.status)}</Badge>
              <Button variant="ghost" size="icon-xs" title="Preview" disabled={unavailable} onClick={() => setPreview(artifact)}>
                <Eye aria-hidden size={14} />
              </Button>
              <Button variant="ghost" size="icon-xs" title="Download" disabled={unavailable} onClick={() => download(artifact)}>
                <Download aria-hidden size={14} />
              </Button>
            </div>
          );
        })}
      </div>
      {preview ? <ArtifactPreview runnerId={runnerId} sessionPk={sessionPk} artifact={preview} onClose={() => setPreview(null)} /> : null}
    </SettingsCard>
  );
}
