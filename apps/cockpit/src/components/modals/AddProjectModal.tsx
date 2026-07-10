import { useEffect, useState } from "react";
import { commands } from "@/bindings";
import { useStore } from "@/store";
import { PROJECTS_ROOT_KEY } from "@/constants";
import { Button, ChoiceCard, FormField, Input, Modal, ModalBody, ModalFooter, ModalHeader, RadioGroup } from "@ryuzi/ui";

type Mode = "folder" | "clone";

const MODES: { id: Mode; label: string; description: string }[] = [
  { id: "folder", label: "Open folder", description: "Use an existing local folder, with or without Git." },
  { id: "clone", label: "Clone from URL", description: "Clone a Git repository into the Projects folder." },
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
    <Modal onClose={onClose} width={440} busy={busy}>
      <ModalHeader title="New project" description="Choose how to add the project to Cockpit." />
      <ModalBody>
        <RadioGroup aria-label="Source" value={mode} onValueChange={(value) => setMode(value as Mode)} className="grid-cols-2">
          {MODES.map((item) => (
            <ChoiceCard key={item.id} value={item.id} title={item.label} description={item.description} />
          ))}
        </RadioGroup>
        {mode === "clone" && (
          <div className="mt-4 flex flex-col gap-3">
            <FormField label="Repository URL">
              <Input value={url} onChange={(event) => setUrl(event.target.value)} placeholder="https://github.com/user/repo.git" />
            </FormField>
            <FormField label="Destination" hint="The repository is cloned into a folder named after it inside this directory.">
              <Input value={dest} onChange={(event) => setDest(event.target.value)} placeholder="Projects folder" />
            </FormField>
          </div>
        )}
      </ModalBody>
      <ModalFooter>
        {mode === "clone" && (
          <Button type="button" variant="outline" onClick={() => void browseDest()}>
            Browse destination
          </Button>
        )}
        <div className="flex-1" />
        <Button variant="outline" disabled={busy} onClick={onClose}>
          Cancel
        </Button>
        {mode === "folder" ? (
          <Button onClick={() => void openFolder()} disabled={busy}>
            {busy ? "Opening..." : "Choose folder"}
          </Button>
        ) : (
          <Button onClick={() => void clone()} disabled={busy || !url.trim() || !dest.trim()}>
            {busy ? "Cloning..." : "Clone"}
          </Button>
        )}
      </ModalFooter>
    </Modal>
  );
}
