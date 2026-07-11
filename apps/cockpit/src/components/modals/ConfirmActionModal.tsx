import { useRef, useState } from "react";
import { Button, Modal, ModalBody, ModalFooter, ModalHeader } from "@ryuzi/ui";

export function ConfirmActionModal({
  open,
  title,
  description,
  confirmLabel,
  busyLabel,
  destructive = true,
  trigger,
  onClose,
  onConfirm,
}: {
  open: boolean;
  title: string;
  description: React.ReactNode;
  confirmLabel: string;
  busyLabel?: string;
  destructive?: boolean;
  trigger: HTMLElement | null;
  onClose: () => void;
  onConfirm: () => Promise<boolean>;
}) {
  const [busy, setBusy] = useState(false);
  const cancelRef = useRef<HTMLButtonElement>(null);
  const finalFocusRef = useRef<HTMLElement | null>(trigger);
  if (!open) return null;
  finalFocusRef.current = trigger;

  const confirm = async () => {
    if (busy) return;
    setBusy(true);
    const close = await onConfirm();
    setBusy(false);
    if (close) onClose();
  };

  return (
    <Modal onClose={onClose} width={420} busy={busy} initialFocus={cancelRef} finalFocus={finalFocusRef}>
      <ModalHeader title={title} />
      <ModalBody>
        <div className="text-[13px] leading-5 text-muted-foreground">{description}</div>
      </ModalBody>
      <ModalFooter>
        <Button ref={cancelRef} variant="outline" onClick={onClose} disabled={busy}>
          Cancel
        </Button>
        <Button variant={destructive ? "destructive" : "default"} onClick={() => void confirm()} disabled={busy}>
          {busy ? (busyLabel ?? `${confirmLabel}…`) : confirmLabel}
        </Button>
      </ModalFooter>
    </Modal>
  );
}
