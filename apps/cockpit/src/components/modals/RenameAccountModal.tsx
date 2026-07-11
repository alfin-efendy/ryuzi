import { useEffect, useRef, useState } from "react";
import type { ConnectionInfo } from "@/bindings";
import { Button, FormField, Input, Modal, ModalBody, ModalFooter, ModalHeader } from "@ryuzi/ui";

export function RenameAccountModal({
  open,
  connection,
  onClose,
  onRename,
}: {
  open: boolean;
  connection: ConnectionInfo | null;
  onClose: () => void;
  onRename: (name: string) => Promise<boolean>;
}) {
  const [name, setName] = useState("");
  const [saving, setSaving] = useState(false);
  const inputRef = useRef<HTMLInputElement>(null);

  useEffect(() => {
    if (open && connection) {
      setName(connection.label || connection.providerName);
      setSaving(false);
    }
  }, [open, connection]);

  if (!open || !connection) return null;

  const original = (connection.label || connection.providerName).trim();
  const trimmed = name.trim();
  const canSave = trimmed.length > 0 && trimmed !== original && !saving;

  const save = async () => {
    if (!canSave) return;
    setSaving(true);
    const ok = await onRename(trimmed);
    setSaving(false);
    if (ok) onClose();
  };

  return (
    <Modal onClose={onClose} width={420} busy={saving} initialFocus={inputRef}>
      <ModalHeader title="Rename account" description={connection.providerName} />
      <ModalBody>
        <FormField label="Account name">
          <Input
            ref={inputRef}
            value={name}
            onChange={(event) => setName(event.target.value)}
            onKeyDown={(event) => {
              if (event.key === "Enter") void save();
            }}
          />
        </FormField>
      </ModalBody>
      <ModalFooter>
        <Button variant="outline" onClick={onClose} disabled={saving}>
          Cancel
        </Button>
        <Button onClick={() => void save()} disabled={!canSave}>
          {saving ? "Saving…" : "Save"}
        </Button>
      </ModalFooter>
    </Modal>
  );
}
