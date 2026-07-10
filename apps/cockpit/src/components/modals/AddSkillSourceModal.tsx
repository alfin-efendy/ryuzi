import { useState } from "react";
import { Button, FormField, Input, Modal, ModalFooter } from "@ryuzi/ui";
import { useSkills } from "@/store-skills";

// Manual skill install (any GitHub repo) — the hand-wired counterpart to
// curated skill packs in Browse, mirroring "Add MCP server" for MCP apps.
export function AddSkillSourceModal({ onClose }: { onClose: () => void }) {
  const installSource = useSkills((s) => s.installSource);
  const loading = useSkills((s) => s.loading);
  const [source, setSource] = useState("");
  const [busy, setBusy] = useState(false);

  const submit = async () => {
    setBusy(true);
    const ok = await installSource(source.trim());
    setBusy(false);
    if (ok) onClose();
  };

  return (
    <Modal onClose={onClose} width={440}>
      <div className="mb-3 text-[15px] font-semibold tracking-[-0.01em]">Add skill source</div>
      <FormField label="Skill source" hint="A GitHub repo (owner/repo) containing agent skills.">
        <Input value={source} onChange={(e) => setSource(e.target.value)} placeholder="owner/repo" aria-label="Skill source" />
      </FormField>
      <ModalFooter>
        <Button variant="outline" onClick={onClose}>
          Cancel
        </Button>
        <Button onClick={() => void submit()} disabled={busy || loading || source.trim() === ""}>
          Install
        </Button>
      </ModalFooter>
    </Modal>
  );
}
