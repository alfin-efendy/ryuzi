import { Radio } from "lucide-react";
import { useState } from "react";
import { useGateways } from "@/store-gateways";
import { Button, FormField, Input, Modal, ModalFooter } from "@ryuzi/ui";

// Pair a remote runner: the user runs `ryuzi pair` on the REMOTE host to get
// a pairing code + the runner's address + cert fingerprint, then enters them
// here. Cockpit pairs over pinned TLS, persists the runner, and connects it
// live — no restart needed. The device token minted along the way never
// reaches this component (or any webview code): it stays in the Tauri
// backend end to end.
export function AddRunnerModal({ onClose }: { onClose: () => void }) {
  const addRunner = useGateways((s) => s.addRunner);
  const [name, setName] = useState("");
  const [host, setHost] = useState("");
  const [port, setPort] = useState("7443");
  const [fingerprint, setFingerprint] = useState("");
  const [code, setCode] = useState("");
  const [saving, setSaving] = useState(false);

  const valid =
    host.trim().length > 0 && Number(port) > 0 && Number(port) < 65536 && fingerprint.trim().length > 0 && code.trim().length > 0;

  const submit = async () => {
    if (!valid || saving) return;
    setSaving(true);
    const ok = await addRunner(name.trim() || host.trim(), host.trim(), Number(port), fingerprint.trim(), code.trim());
    setSaving(false);
    if (ok) onClose();
  };

  return (
    <Modal onClose={onClose} width={440}>
      <div className="mb-1 flex items-center gap-2.5">
        <Radio aria-hidden size={16} strokeWidth={2} className="text-muted-foreground" />
        <span className="text-[15px] font-semibold tracking-[-0.01em]">Add runner</span>
      </div>
      <p className="mb-[18px] mt-0 text-[12.5px] text-muted-foreground">
        Run <code className="rounded bg-muted px-1 py-px font-mono text-[11.5px]">ryuzi pair</code> on the remote host, then enter the code
        and address it prints here. Cockpit pairs over TLS and connects the runner immediately.
      </p>
      <div className="flex flex-col gap-3">
        <FormField label="Name">
          <Input value={name} onChange={(e) => setName(e.target.value)} placeholder="gpu-box" />
        </FormField>
        <div className="flex gap-3">
          <FormField label="Host" className="flex-[2]">
            <Input value={host} onChange={(e) => setHost(e.target.value)} placeholder="10.0.0.9" />
          </FormField>
          <FormField label="Port" className="flex-1">
            <Input value={port} onChange={(e) => setPort(e.target.value)} placeholder="7443" />
          </FormField>
        </div>
        <FormField label="Fingerprint">
          <Input
            value={fingerprint}
            onChange={(e) => setFingerprint(e.target.value)}
            placeholder="SHA-256 fingerprint from ryuzi pair"
            className="font-mono text-xs"
          />
        </FormField>
        <FormField label="Pairing code">
          <Input value={code} onChange={(e) => setCode(e.target.value)} placeholder="Code from ryuzi pair" className="font-mono text-xs" />
        </FormField>
      </div>
      <ModalFooter>
        <Button variant="outline" onClick={onClose}>
          Cancel
        </Button>
        <Button disabled={!valid || saving} onClick={() => void submit()}>
          {saving ? "Pairing…" : "Pair runner"}
        </Button>
      </ModalFooter>
    </Modal>
  );
}
