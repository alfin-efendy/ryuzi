import { useEffect, useState } from "react";
import { Button, FormField, Input, Modal, ModalFooter } from "@ryuzi/ui";
import { newBranchNameError, normalizeBranchName } from "@/lib/composer-git";

/** Names a new branch for the composer. On Create it only hands the name to
 *  the caller (pending create intent) — no git command runs here. */
export function BranchNameModal({
  open,
  onClose,
  existingBranches,
  onCreate,
}: {
  open: boolean;
  onClose: () => void;
  existingBranches: string[];
  onCreate: (name: string) => void;
}) {
  const [name, setName] = useState("");

  useEffect(() => {
    if (open) setName("");
  }, [open]);

  if (!open) return null;

  // Live normalization can leave leading/trailing dashes (e.g. pasted
  // "  feat/login " becomes "-feat/login-"); strip them for validation/submit.
  const trimmed = name.trim().replace(/^-+|-+$/g, "");
  const error = newBranchNameError(trimmed, existingBranches);
  const submit = () => {
    if (error !== null) return;
    onCreate(trimmed);
    onClose();
  };

  return (
    <Modal onClose={onClose} width={400}>
      <div className="text-[15px] font-semibold tracking-[-0.01em]">New Branch</div>
      <div className="mt-3.5">
        <FormField label="Branch name">
          <Input
            autoFocus
            value={name}
            onChange={(e) => setName(normalizeBranchName(e.target.value))}
            onKeyDown={(e) => {
              if (e.key === "Enter") submit();
            }}
            placeholder="feat/my-change"
            className="font-mono text-[12.5px]"
          />
        </FormField>
        {trimmed.length > 0 && error !== null && <p className="mt-1.5 text-[11.5px] text-destructive">{error}</p>}
      </div>
      <ModalFooter>
        <Button variant="outline" onClick={onClose}>
          Cancel
        </Button>
        <Button onClick={submit} disabled={error !== null}>
          Create
        </Button>
      </ModalFooter>
    </Modal>
  );
}
