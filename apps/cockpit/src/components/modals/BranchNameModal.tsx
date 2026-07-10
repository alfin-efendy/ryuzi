import { useEffect, useRef, useState } from "react";
import { Button, FormField, Input, Modal, ModalBody, ModalFooter, ModalHeader } from "@ryuzi/ui";
import { newBranchNameError } from "@/lib/composer-git";

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
  const inputRef = useRef<HTMLInputElement>(null);

  useEffect(() => {
    if (open) setName("");
  }, [open]);

  if (!open) return null;

  const trimmed = name.trim();
  const error = newBranchNameError(trimmed, existingBranches);
  const submit = () => {
    if (error !== null) return;
    onCreate(trimmed);
    onClose();
  };

  return (
    <Modal onClose={onClose} width={400} initialFocus={inputRef}>
      <ModalHeader title="New Branch" />
      <ModalBody>
        <FormField label="Branch name">
          <Input
            ref={inputRef}
            value={name}
            onChange={(event) => setName(event.target.value)}
            onKeyDown={(event) => {
              if (event.key === "Enter") submit();
            }}
            placeholder="feat/my-change"
            className="font-mono text-[12.5px]"
          />
        </FormField>
        {trimmed.length > 0 && error !== null && <p className="mt-1.5 text-[11.5px] text-destructive">{error}</p>}
      </ModalBody>
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
