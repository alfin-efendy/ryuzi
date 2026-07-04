import { LayoutGrid } from "lucide-react";
import { useState } from "react";
import { useApps } from "@/store-apps";
import { Button, FormField, Input, Modal, ModalFooter, Segmented, Textarea } from "@ryuzi/ui";

// Add an MCP server by hand (stdio command or HTTP URL). Adding runs a real
// handshake, so the card lands with a true status and discovered tool list.
export function AddAppModal({ onClose }: { onClose: () => void }) {
  const add = useApps((s) => s.add);
  const [name, setName] = useState("");
  const [desc, setDesc] = useState("");
  const [transport, setTransport] = useState<"stdio" | "http">("stdio");
  const [command, setCommand] = useState("");
  const [url, setUrl] = useState("");
  const [env, setEnv] = useState("");
  const [saving, setSaving] = useState(false);

  const valid = name.trim().length > 0 && (transport === "stdio" ? command.trim().length > 0 : url.trim().length > 0);

  const submit = async () => {
    if (!valid || saving) return;
    setSaving(true);
    const parts = command.trim().split(/\s+/);
    const ok = await add({
      id: null,
      name: name.trim(),
      description: desc.trim(),
      kind: "MCP server",
      transport,
      command: transport === "stdio" ? (parts[0] ?? "") : null,
      args: transport === "stdio" ? parts.slice(1) : [],
      env: env
        .split("\n")
        .map((l) => l.trim())
        .filter((l) => l.includes("=")),
      url: transport === "http" ? url.trim() : null,
      version: null,
      publisher: null,
      color: null,
    });
    setSaving(false);
    if (ok) onClose();
  };

  return (
    <Modal onClose={onClose} width={480}>
      <div className="mb-1 flex items-center gap-2.5">
        <LayoutGrid aria-hidden size={16} strokeWidth={2} className="text-muted-foreground" />
        <span className="text-[15px] font-semibold tracking-[-0.01em]">Add app</span>
      </div>
      <p className="mb-[18px] mt-0 text-[12.5px] text-muted-foreground">
        Point Cockpit at an MCP server. It connects immediately to verify and discover the tool list.
      </p>
      <div className="flex flex-col gap-3">
        <div className="flex gap-3">
          <FormField label="Name" className="flex-1">
            <Input value={name} onChange={(e) => setName(e.target.value)} placeholder="GitHub" />
          </FormField>
          <div className="flex flex-col gap-1.5">
            <span className="text-xs font-semibold">Transport</span>
            <Segmented
              options={[
                { id: "stdio", label: "Stdio" },
                { id: "http", label: "HTTP" },
              ]}
              value={transport}
              onChange={setTransport}
            />
          </div>
        </div>
        <FormField label="Description">
          <Input value={desc} onChange={(e) => setDesc(e.target.value)} placeholder="What agents use it for" />
        </FormField>
        {transport === "stdio" ? (
          <FormField label="Command">
            <Input
              className="font-mono text-xs"
              value={command}
              onChange={(e) => setCommand(e.target.value)}
              placeholder="npx -y @modelcontextprotocol/server-github"
            />
          </FormField>
        ) : (
          <FormField label="URL">
            <Input
              className="font-mono text-xs"
              value={url}
              onChange={(e) => setUrl(e.target.value)}
              placeholder="https://mcp.example.com"
            />
          </FormField>
        )}
        <FormField label="Environment (KEY=value, one per line)">
          <Textarea
            className="resize-y font-mono text-xs"
            value={env}
            onChange={(e) => setEnv(e.target.value)}
            placeholder="GITHUB_TOKEN=ghp_…"
          />
        </FormField>
      </div>
      <ModalFooter>
        <Button variant="outline" onClick={onClose}>
          Cancel
        </Button>
        <Button disabled={!valid || saving} onClick={() => void submit()}>
          {saving ? "Connecting…" : "Add & connect"}
        </Button>
      </ModalFooter>
    </Modal>
  );
}
