import { Server } from "lucide-react";
import { useState } from "react";
import { useGateways } from "@/store-gateways";
import { Modal } from "./Modal";

const field =
  "h-[34px] w-full rounded-md border border-input bg-background px-3 font-sans text-[13px] text-foreground";

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
    <Modal onClose={onClose} width={440}>
      <div className="mb-1 flex items-center gap-2.5">
        <Server aria-hidden size={16} strokeWidth={2} className="text-muted-foreground" />
        <span className="text-[15px] font-semibold tracking-[-0.01em]">Connect gateway</span>
      </div>
      <p className="mb-[18px] mt-0 text-[12.5px] text-muted-foreground">
        Add an SSH host. Cockpit records it and probes reachability; running sessions remotely arrives with the gateway daemon.
      </p>
      <div className="flex flex-col gap-3">
        <label className="flex flex-col gap-1.5">
          <span className="text-xs font-semibold">Name</span>
          <input className={field} value={name} onChange={(e) => setName(e.target.value)} placeholder="prod-sg1" />
        </label>
        <div className="flex gap-3">
          <label className="flex flex-[2] flex-col gap-1.5">
            <span className="text-xs font-semibold">Host</span>
            <input className={field} value={host} onChange={(e) => setHost(e.target.value)} placeholder="128.140.42.7" />
          </label>
          <label className="flex flex-1 flex-col gap-1.5">
            <span className="text-xs font-semibold">Port</span>
            <input className={field} value={port} onChange={(e) => setPort(e.target.value)} placeholder="22" />
          </label>
        </div>
        <label className="flex flex-col gap-1.5">
          <span className="text-xs font-semibold">User</span>
          <input className={field} value={username} onChange={(e) => setUsername(e.target.value)} placeholder="deploy" />
        </label>
      </div>
      <div className="mt-[22px] flex items-center justify-end gap-2">
        <button
          type="button"
          onClick={onClose}
          className="h-8 cursor-pointer rounded-md border border-border bg-transparent px-3.5 font-sans text-[12.5px] font-medium text-foreground hover:bg-accent"
        >
          Cancel
        </button>
        <button
          type="button"
          disabled={!valid || saving}
          onClick={() => void submit()}
          className="h-8 cursor-pointer rounded-md border-none bg-primary px-3.5 font-sans text-[12.5px] font-medium text-primary-foreground hover:opacity-85 disabled:opacity-50"
        >
          {saving ? "Probing…" : "Connect"}
        </button>
      </div>
    </Modal>
  );
}
