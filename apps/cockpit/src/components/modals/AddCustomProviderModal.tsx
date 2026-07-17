import { useState } from "react";
import { Button, FormField, Input, Modal, ModalBody, ModalFooter, ModalHeader } from "@ryuzi/ui";

// Names the new custom provider. The OpenAI-/Anthropic-compatible choice and
// the base URL are collected later, per account, in the Add Account modal.
export function AddCustomProviderModal({
  open,
  onClose,
  onCreate,
}: {
  open: boolean;
  onClose: () => void;
  onCreate: (name: string) => Promise<boolean>;
}) {
  const [name, setName] = useState("");
  const [saving, setSaving] = useState(false);

  if (!open) return null;

  const close = () => {
    setName("");
    setSaving(false);
    onClose();
  };

  const submit = async () => {
    if (!name.trim() || saving) return;
    setSaving(true);
    const ok = await onCreate(name.trim());
    setSaving(false);
    if (ok) {
      setName("");
      onClose();
    }
  };

  return (
    <Modal onClose={close} width={420} busy={saving}>
      <ModalHeader title="Add custom provider" />
      <ModalBody>
        <FormField label="Provider name">
          <Input value={name} onChange={(event) => setName(event.target.value)} placeholder="My Gateway" />
        </FormField>
        <p className="mt-2 text-xs text-muted-foreground">
          You'll choose OpenAI- or Anthropic-compatible and enter the base URL when you add an account.
        </p>
      </ModalBody>
      <ModalFooter>
        <div className="flex-1" />
        <Button variant="outline" disabled={saving} onClick={close}>
          Cancel
        </Button>
        <Button disabled={!name.trim() || saving} onClick={() => void submit()}>
          {saving ? "Adding..." : "Create"}
        </Button>
      </ModalFooter>
    </Modal>
  );
}
