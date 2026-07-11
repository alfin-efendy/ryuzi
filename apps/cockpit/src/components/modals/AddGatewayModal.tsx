import { Server } from "lucide-react";
import { useState } from "react";
import { useGateways } from "@/store-gateways";
import { Button, FormField, Input, Modal, ModalBody, ModalFooter, ModalHeader } from "@ryuzi/ui";

// Connect an SSH gateway: persisted config + TCP reachability probe. Remote
// execution lands with the daemon; until then this is a monitoring entry.
export function AddGatewayModal({ onClose }: { onClose: () => void }) {
  const add = useGateways((s) => s.add);
  const [name, setName] = useState("");
  const [host, setHost] = useState("");
  const [port, setPort] = useState("22");
  const [username, setUsername] = useState("");
  const [saving, setSaving] = useState(false);

  const valid = host.trim().length > 0 && Number(port) > 0 && Number(port) < 65536;

  const submit = async () => {
    if (!valid || saving) return;
    setSaving(true);
    const ok = await add(name.trim() || host.trim(), host.trim(), Number(port), username.trim());
    setSaving(false);
    if (ok) onClose();
  };

  return (
    <Modal onClose={onClose} width={440} busy={saving}>
      <ModalHeader
        leading={<Server aria-hidden className="mt-0.5 size-4 text-muted-foreground" strokeWidth={2} />}
        title="Connect gateway"
        description="Add an SSH host. Cockpit records it and probes reachability; running sessions remotely arrives with the gateway daemon."
      />
      <ModalBody>
        <div className="flex flex-col gap-3">
          <FormField label="Name">
            <Input value={name} onChange={(e) => setName(e.target.value)} placeholder="prod-sg1" />
          </FormField>
          <div className="flex gap-3">
            <FormField label="Host" className="flex-[2]">
              <Input value={host} onChange={(e) => setHost(e.target.value)} placeholder="128.140.42.7" />
            </FormField>
            <FormField label="Port" className="flex-1">
              <Input value={port} onChange={(e) => setPort(e.target.value)} placeholder="22" />
            </FormField>
          </div>
          <FormField label="User">
            <Input value={username} onChange={(e) => setUsername(e.target.value)} placeholder="deploy" />
          </FormField>
        </div>
      </ModalBody>
      <ModalFooter>
        <Button variant="outline" disabled={saving} onClick={onClose}>
          Cancel
        </Button>
        <Button disabled={!valid || saving} onClick={() => void submit()}>
          {saving ? "Probing…" : "Connect"}
        </Button>
      </ModalFooter>
    </Modal>
  );
}
