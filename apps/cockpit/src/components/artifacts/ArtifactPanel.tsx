import { useCallback, useEffect, useState } from "react";
import { Download, Eye, FileText, Link2, RefreshCw } from "lucide-react";
import { Badge, Button } from "@ryuzi/ui";
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

/** Session artifacts live in the right panel so the chat transcript remains a
 * focused reading surface. */
export function ArtifactPanel({ runnerId, sessionPk, refreshKey }: Props) {
  const [items, setItems] = useState<ArtifactInfo[]>([]);
  const [error, setError] = useState<string | null>(null);
  const [preview, setPreview] = useState<ArtifactInfo | null>(null);

  const load = useCallback(() => {
    if (!commands.listSessionArtifacts) return;
    void commands.listSessionArtifacts(runnerId, sessionPk).then((result) => {
      if (result.status === "ok") {
        setItems(result.data);
        setError(null);
      } else setError(result.error.message);
    });
  }, [runnerId, sessionPk]);

  useEffect(() => {
    load();
  }, [load]);

  useEffect(() => {
    if (refreshKey === undefined) return;
    load();
  }, [load, refreshKey]);

  const download = (artifact: ArtifactInfo) => {
    if (!commands.fetchArtifact) return;
    void commands.fetchArtifact(runnerId, sessionPk, artifact.id).then((result) => {
      if (result.status === "ok") save(result.data);
      else setError(result.error.message);
    });
  };

  return (
    <section className="flex min-h-0 flex-1 flex-col" aria-label="Artifacts">
      <div className="flex shrink-0 items-center gap-2 border-b border-border px-4 py-3">
        <div className="flex size-7 items-center justify-center rounded-md bg-sky-500/10 text-sky-600 dark:text-sky-400">
          <FileText aria-hidden size={14} />
        </div>
        <div className="min-w-0 flex-1">
          <h2 className="text-[13px] font-semibold">Artifacts</h2>
          <p className="text-[12px] text-muted-foreground">
            {items.length === 0 ? "Files created in this session" : `${items.length} file${items.length === 1 ? "" : "s"} available`}
          </p>
        </div>
        <Button variant="ghost" size="icon-sm" title="Refresh artifacts" onClick={load} className="text-muted-foreground">
          <RefreshCw aria-hidden size={14} />
        </Button>
      </div>
      {error ? (
        <div role="alert" className="shrink-0 border-b border-destructive/30 bg-destructive/5 px-4 py-2 text-xs text-destructive">
          {error}
        </div>
      ) : null}
      {items.length === 0 ? (
        <div className="flex flex-1 items-center justify-center px-6 text-center text-[12.5px] text-muted-foreground">
          No artifacts in this session.
        </div>
      ) : (
        <div className="min-h-0 flex-1 divide-y divide-border overflow-y-auto">
          {items.map((artifact) => {
            const unavailable = artifact.status === "deleted";
            return (
              <div
                key={`${artifact.id}:${artifact.referenceId ?? "source"}`}
                className="flex min-h-14 items-center gap-2.5 px-4 py-2.5 text-xs"
              >
                <FileText aria-hidden size={15} className="shrink-0 text-muted-foreground" />
                <div className="min-w-0 flex-1">
                  <div className="truncate font-medium text-foreground">{artifact.name}</div>
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
                <div className="flex shrink-0 items-center">
                  <Button variant="ghost" size="icon-xs" title="Preview" disabled={unavailable} onClick={() => setPreview(artifact)}>
                    <Eye aria-hidden size={14} />
                  </Button>
                  <Button variant="ghost" size="icon-xs" title="Download" disabled={unavailable} onClick={() => download(artifact)}>
                    <Download aria-hidden size={14} />
                  </Button>
                </div>
              </div>
            );
          })}
        </div>
      )}
      {preview ? <ArtifactPreview runnerId={runnerId} sessionPk={sessionPk} artifact={preview} onClose={() => setPreview(null)} /> : null}
    </section>
  );
}
