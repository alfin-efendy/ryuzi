import { useEffect, useState } from "react";
import { Download } from "lucide-react";
import { Button, Modal, ModalBody, ModalFooter, ModalHeader } from "@ryuzi/ui";
import { commands, type ArtifactFileInfo, type ArtifactInfo } from "@/bindings";

type Props = { runnerId: string; sessionPk: string; artifact: ArtifactInfo; onClose: () => void };

function isImage(contentType: string | null) {
  return contentType?.startsWith("image/") ?? false;
}

function isText(contentType: string | null, name: string) {
  return contentType?.startsWith("text/") || /\.(md|markdown|txt|json|ya?ml|toml|rs|ts|tsx|js|jsx|py|go|java|css|html)$/i.test(name);
}

function download(file: ArtifactFileInfo) {
  const bytes = Uint8Array.from(atob(file.dataBase64), (char) => char.charCodeAt(0));
  const url = URL.createObjectURL(new Blob([bytes], { type: file.contentType ?? "application/octet-stream" }));
  const anchor = document.createElement("a");
  anchor.href = url;
  anchor.download = file.name;
  anchor.click();
  URL.revokeObjectURL(url);
}

export function ArtifactPreview({ runnerId, sessionPk, artifact, onClose }: Props) {
  const [file, setFile] = useState<ArtifactFileInfo | null>(null);
  const [error, setError] = useState<string | null>(null);

  useEffect(() => {
    let cancelled = false;
    void commands.fetchArtifact(runnerId, sessionPk, artifact.id).then((result) => {
      if (cancelled) return;
      if (result.status === "ok") setFile(result.data);
      else setError(result.error.message);
    });
    return () => { cancelled = true; };
  }, [artifact.id, runnerId, sessionPk]);

  const body = () => {
    if (error) return <p className="text-sm text-destructive">{error}</p>;
    if (!file) return <p className="text-sm text-muted-foreground">Loading artifact…</p>;
    if (isImage(file.contentType)) return <img src={`data:${file.contentType};base64,${file.dataBase64}`} alt={file.name} className="max-h-[65vh] max-w-full object-contain" />;
    if (isText(file.contentType, file.name)) {
      const text = new TextDecoder().decode(Uint8Array.from(atob(file.dataBase64), (char) => char.charCodeAt(0)));
      return <pre className="max-h-[60vh] overflow-auto rounded-md bg-muted p-3 text-xs whitespace-pre-wrap">{text}</pre>;
    }
    return <p className="text-sm text-muted-foreground">Preview is unavailable for this file type. Download it to open with another application.</p>;
  };

  return <Modal onClose={onClose} width={760}>
    <ModalHeader title={artifact.name} />
    <ModalBody>{body()}</ModalBody>
    <ModalFooter>
      <Button variant="outline" onClick={onClose}>Close</Button>
      {file ? <Button onClick={() => download(file)}><Download aria-hidden size={14} />Download</Button> : null}
    </ModalFooter>
  </Modal>;
}
