import { useRef, useState } from "react";
import { Button, Modal, ModalBody, ModalFooter, ModalHeader } from "@ryuzi/ui";

export type ConfirmAccountAction =
  | { kind: "delete"; accountName: string; onConfirm: () => Promise<boolean> }
  | { kind: "resetCredit"; accountName: string; onConfirm: () => Promise<boolean> };

export function ConfirmAccountActionModal({
  open,
  action,
  onClose,
}: {
  open: boolean;
  action: ConfirmAccountAction | null;
  onClose: () => void;
}) {
  const [busy, setBusy] = useState(false);
  const cancelRef = useRef<HTMLButtonElement>(null);

  if (!open || !action) return null;

  const deleting = action.kind === "delete";
  const confirm = async () => {
    if (busy) return;
    setBusy(true);
    const ok = await action.onConfirm();
    setBusy(false);
    if (ok) onClose();
  };

  return (
    <Modal onClose={onClose} width={420} busy={busy} initialFocus={cancelRef}>
      <ModalHeader title={deleting ? "Delete account?" : "Reset credit?"} />
      <ModalBody>
        <p className="text-[13px] leading-5 text-muted-foreground">
          {deleting
            ? `Delete ${action.accountName}? This account will be removed and cannot be undone.`
            : `Spend one reset credit for ${action.accountName}?`}
        </p>
      </ModalBody>
      <ModalFooter>
        <Button ref={cancelRef} variant="outline" onClick={onClose} disabled={busy}>
          Cancel
        </Button>
        <Button
          data-variant={deleting ? "destructive" : "default"}
          variant={deleting ? "destructive" : "default"}
          onClick={() => void confirm()}
          disabled={busy}
        >
          {busy ? (deleting ? "Deleting…" : "Resetting…") : deleting ? "Delete account" : "Reset credit"}
        </Button>
      </ModalFooter>
    </Modal>
  );
}
