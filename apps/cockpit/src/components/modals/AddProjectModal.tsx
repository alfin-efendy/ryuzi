import { useEffect, useState } from "react";
import { commands } from "@/bindings";
import { useStore } from "@/store";
import { PROJECTS_ROOT_KEY } from "@/constants";
import { Button, FormField, Input, Modal, ModalFooter } from "@ryuzi/ui";

type Mode = "folder" | "clone";

const MODES: { id: Mode; label: string }[] = [
  { id: "folder", label: "Open folder" },
  { id: "clone", label: "Clone from URL" },
];

export function AddProjectModal({ open, onClose }: { open: boolean; onClose: () => void }) {
  const addProject = useStore((s) => s.addProject);
  const cloneProject = useStore((s) => s.cloneProject);
  const [mode, setMode] = useState<Mode>("folder");
  const [url, setUrl] = useState("");
  const [dest, setDest] = useState("");
  const [busy, setBusy] = useState(false);

  useEffect(() => {
    if (!open) return;
    setMode("folder");
    setUrl("");
    setBusy(false);
    // Default clone destination = the persisted "Projects folder" setting;
    // Browse below overrides it for this clone only.
    void commands.getSetting(PROJECTS_ROOT_KEY).then((res) => {
      setDest(res.status === "ok" ? (res.data ?? "") : "");
    });
  }, [open]);

  if (!open) return null;

  const openFolder = async () => {
    if (busy) return;
    setBusy(true);
    const ok = await addProject();
    setBusy(false);
    if (ok) onClose(); // picker cancel / connect failure keeps the modal open
  };

  const browseDest = async () => {
    const dir = await commands.pickDirectory();
    if (dir) setDest(dir);
  };

  const clone = async () => {
    if (busy || !url.trim() || !dest.trim()) return;
    setBusy(true);
    const ok = await cloneProject(url.trim(), dest.trim());
    setBusy(false);
    if (ok) onClose();
  };

  return (
    <Modal onClose={onClose} width={440}>
      <div className="text-[15px] font-semibold tracking-[-0.01em]">New project</div>

      <div role="radiogroup" aria-label="Source" className="mt-4 grid grid-cols-2 gap-2">
        {MODES.map((m) => (
          <Button
            key={m.id}
            role="radio"
            aria-checked={mode === m.id}
            variant={mode === m.id ? "secondary" : "outline"}
            onClick={() => setMode(m.id)}
          >
            {m.label}
          </Button>
        ))}
      </div>

      {mode === "folder" ? (
        <>
          <p className="mt-3.5 text-[12.5px] text-muted-foreground">
            Add an existing folder as a project. Folders without a git repository work too — git features are disabled for them.
          </p>
          <Button size="lg" onClick={() => void openFolder()} disabled={busy} className="mt-3.5 w-full">
            {busy ? "Opening..." : "Choose folder"}
          </Button>
        </>
      ) : (
        <>
          <div className="mt-3.5 flex flex-col gap-3">
            <FormField label="Repository URL">
              <Input value={url} onChange={(e) => setUrl(e.target.value)} placeholder="https://github.com/user/repo.git" />
            </FormField>
            <FormField label="Destination" hint="Clones into a folder named after the repository inside this directory.">
              <div className="flex min-w-0 gap-2">
                <Input value={dest} onChange={(e) => setDest(e.target.value)} placeholder="Projects folder" className="min-w-0 flex-1" />
                <Button type="button" variant="outline" aria-label="Browse" onClick={() => void browseDest()} className="shrink-0">
                  Browse
                </Button>
              </div>
            </FormField>
          </div>
          <Button size="lg" onClick={() => void clone()} disabled={busy || !url.trim() || !dest.trim()} className="mt-3.5 w-full">
            {busy ? "Cloning..." : "Clone"}
          </Button>
        </>
      )}

      <ModalFooter className="mt-4">
        <Button variant="outline" onClick={onClose}>
          Cancel
        </Button>
      </ModalFooter>
    </Modal>
  );
}
