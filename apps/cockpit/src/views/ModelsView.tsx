import { useEffect, useState } from "react";
import { ChevronDown, ChevronRight, ChevronUp, Copy, Plus } from "lucide-react";
import { toast } from "sonner";
import { useEndpoint } from "@/store-endpoint";
import { useConnections } from "@/store-connections";
import { useNav } from "@/store-nav";
import type { ConnectionInfo } from "@/bindings";
import { Card, CardHeader, CardHint, CardRow, CardTitle } from "@/components/common/Card";
import { Chip, StatusDot } from "@/components/common/bits";
import { Segmented } from "@/components/common/Segmented";
import { Switch } from "@/components/common/Switch";
import { AddConnectionModal } from "@/components/modals/AddConnectionModal";

type Tab = "endpoint" | "connections";

const iconBtn =
  "flex h-7 w-7 cursor-pointer items-center justify-center rounded-md border-none bg-transparent text-muted-foreground hover:bg-accent";
const field = "h-9 rounded-md border border-input bg-background px-3 font-sans text-[12.5px] text-foreground";
const moveBtn =
  "flex h-[15px] w-5 cursor-pointer items-center justify-center border-none bg-transparent p-0 text-muted-foreground hover:text-foreground";

function EndpointTab() {
  const { status, keys, start, stop, setConfig, createKey, revokeKey } = useEndpoint();
  const [port, setPort] = useState("");
  const [autostart, setAutostart] = useState(false);
  const [portInit, setPortInit] = useState(false);
  const [busy, setBusy] = useState(false);
  const [savingConfig, setSavingConfig] = useState(false);
  const [keyName, setKeyName] = useState("");
  const [creatingKey, setCreatingKey] = useState(false);

  // Seed the settings form from the first status load only — later refreshes
  // (e.g. after Start/Stop) shouldn't clobber an in-progress edit.
  useEffect(() => {
    if (status && !portInit) {
      setPort(String(status.port));
      setAutostart(status.autostart);
      setPortInit(true);
    }
  }, [status, portInit]);

  const toggle = async () => {
    setBusy(true);
    if (status?.running) await stop();
    else await start();
    setBusy(false);
  };

  const copyBaseUrl = () => {
    if (!status) return;
    void navigator.clipboard.writeText(status.baseUrl);
    toast.success("Copied");
  };

  const saveConfig = async () => {
    const p = Number(port);
    if (!Number.isFinite(p) || p <= 0) {
      toast.error("Enter a valid port");
      return;
    }
    setSavingConfig(true);
    await setConfig(p, autostart);
    setSavingConfig(false);
  };

  const submitKey = async () => {
    if (!keyName.trim() || creatingKey) return;
    setCreatingKey(true);
    await createKey(keyName.trim());
    setCreatingKey(false);
    setKeyName("");
  };

  const doRevoke = async (id: string, name: string) => {
    if (!window.confirm(`Revoke key "${name}"? Apps using it will lose access immediately.`)) return;
    await revokeKey(id);
  };

  return (
    <div className="flex flex-col gap-3">
      <Card>
        <div className="flex items-center gap-3 border-b border-border px-[18px] py-3.5">
          <StatusDot color={status?.running ? "#22C55E" : "var(--muted-foreground)"} pulse={!!status?.running} size={8} />
          <div className="min-w-0 flex-1">
            <div className="text-sm font-semibold text-foreground">{status?.running ? `Running on ${status.baseUrl}` : "Stopped"}</div>
            <div className="mt-0.5 text-xs text-muted-foreground">Local OpenAI-compatible endpoint for external tools.</div>
          </div>
          <button
            type="button"
            onClick={() => void toggle()}
            disabled={busy || !status}
            className="h-8 shrink-0 cursor-pointer rounded-md border border-border bg-transparent px-3.5 font-sans text-[12.5px] font-medium text-foreground hover:bg-accent disabled:opacity-50"
          >
            {status?.running ? "Stop" : "Start"}
          </button>
        </div>
        <div className="flex items-center gap-2 px-[18px] py-3">
          <span className="min-w-0 flex-1 truncate font-mono text-xs text-muted-foreground">{status?.baseUrl ?? "—"}</span>
          <button type="button" title="Copy base URL" onClick={copyBaseUrl} className={iconBtn}>
            <Copy aria-hidden size={13} strokeWidth={2} />
          </button>
        </div>
      </Card>

      <Card>
        <CardHeader>
          <CardTitle>Settings</CardTitle>
        </CardHeader>
        <CardRow>
          <span className="w-28 shrink-0 text-[13px] font-medium">Port</span>
          <input type="number" className={`${field} w-28`} value={port} onChange={(e) => setPort(e.target.value)} />
        </CardRow>
        <CardRow>
          <label className="flex flex-1 items-center gap-2 text-[13px] font-medium">
            <input
              type="checkbox"
              checked={autostart}
              onChange={(e) => setAutostart(e.target.checked)}
              className="h-4 w-4 accent-primary"
            />
            Start automatically with Cockpit
          </label>
        </CardRow>
        <div className="flex justify-end px-[18px] py-3">
          <button
            type="button"
            onClick={() => void saveConfig()}
            disabled={savingConfig}
            className="h-8 cursor-pointer rounded-md border-none bg-primary px-3.5 font-sans text-[12.5px] font-medium text-primary-foreground hover:opacity-85 disabled:opacity-50"
          >
            {savingConfig ? "Saving…" : "Save"}
          </button>
        </div>
      </Card>

      <Card>
        <CardHeader>
          <CardTitle>API keys</CardTitle>
          <CardHint>Required by external tools calling the local endpoint</CardHint>
        </CardHeader>
        {keys.map((k) => (
          <div key={k.id} className="flex items-center gap-3 border-b border-border px-[18px] py-3 last:border-b-0">
            <div className="min-w-0 flex-1">
              <div className="text-[13px] font-semibold">{k.name}</div>
              <div className="mt-1 flex items-center gap-1.5">
                <span className="truncate font-mono text-xs text-muted-foreground">{k.key}</span>
                <button
                  type="button"
                  title="Copy key"
                  onClick={() => {
                    void navigator.clipboard.writeText(k.key);
                    toast.success("Copied");
                  }}
                  className={iconBtn}
                >
                  <Copy aria-hidden size={12} strokeWidth={2} />
                </button>
              </div>
              <div className="mt-1 text-[11px] text-muted-foreground">
                Created {new Date(k.createdAt).toLocaleDateString()} · Last used{" "}
                {k.lastUsedAt ? new Date(k.lastUsedAt).toLocaleDateString() : "never"}
              </div>
            </div>
            <button
              type="button"
              onClick={() => void doRevoke(k.id, k.name)}
              className="h-[27px] shrink-0 cursor-pointer rounded-md border border-border bg-transparent px-[11px] font-sans text-xs font-medium text-destructive hover:bg-accent"
            >
              Revoke
            </button>
          </div>
        ))}
        {keys.length === 0 && (
          <div className="px-[18px] py-3.5 text-[12.5px] text-muted-foreground">No API keys yet — create one for external tools.</div>
        )}
        <div className="flex items-center gap-2 px-[18px] py-3">
          <input
            className={`${field} flex-1`}
            value={keyName}
            onChange={(e) => setKeyName(e.target.value)}
            placeholder="Key name (e.g. VS Code)"
          />
          <button
            type="button"
            onClick={() => void submitKey()}
            disabled={!keyName.trim() || creatingKey}
            className="h-9 shrink-0 cursor-pointer rounded-md border-none bg-primary px-3.5 font-sans text-[12.5px] font-medium text-primary-foreground hover:opacity-85 disabled:opacity-50"
          >
            {creatingKey ? "Creating…" : "New key"}
          </button>
        </div>
      </Card>
    </div>
  );
}

function ConnectionRow({ conn, index, count }: { conn: ConnectionInfo; index: number; count: number }) {
  const nav = useNav();
  const update = useConnections((s) => s.update);
  const move = useConnections((s) => s.move);
  const test = useConnections((s) => s.test);
  const [testing, setTesting] = useState(false);

  const open = () => nav.navigate({ kind: "connectionDetail", id: conn.id });

  const runTest = async () => {
    setTesting(true);
    const result = await test(conn.id);
    setTesting(false);
    if (result) {
      if (result.ok) toast.success(result.message);
      else toast.error(result.message);
    }
  };

  return (
    <div className="flex items-center gap-3 border-b border-border px-[18px] py-3.5 last:border-b-0">
      <div className="flex shrink-0 flex-col items-center gap-px">
        <button
          type="button"
          title="Move up"
          onClick={() => void move(conn.id, -1)}
          className={`${moveBtn} ${index === 0 ? "invisible" : ""}`}
        >
          <ChevronUp aria-hidden size={11} strokeWidth={2.5} />
        </button>
        <span className="flex h-5 w-5 items-center justify-center rounded-full bg-muted font-mono text-[10.5px] font-semibold text-muted-foreground">
          {index + 1}
        </span>
        <button
          type="button"
          title="Move down"
          onClick={() => void move(conn.id, 1)}
          className={`${moveBtn} ${index === count - 1 ? "invisible" : ""}`}
        >
          <ChevronDown aria-hidden size={11} strokeWidth={2.5} />
        </button>
      </div>
      <Chip initial={conn.initial} color={conn.color} size={34} onClick={open} />
      <button type="button" onClick={open} className="min-w-0 flex-1 cursor-pointer border-none bg-transparent p-0 text-left font-sans">
        <span className="block text-sm font-semibold text-foreground">{conn.label || conn.providerName}</span>
        <span className="block text-xs text-muted-foreground">
          {conn.providerName} · {conn.keyMasked ?? "no key"} · {conn.models.length} model{conn.models.length === 1 ? "" : "s"}
        </span>
      </button>
      <Switch
        on={conn.enabled}
        onToggle={() =>
          void update(conn.id, {
            label: conn.label,
            enabled: !conn.enabled,
            apiKey: null,
            baseUrl: conn.baseUrl,
            models: conn.models,
          })
        }
        label="Enabled"
      />
      <button
        type="button"
        onClick={() => void runTest()}
        disabled={testing}
        className="h-[27px] shrink-0 cursor-pointer rounded-md border border-border bg-transparent px-[11px] font-sans text-xs font-medium text-foreground hover:bg-accent disabled:opacity-50"
      >
        {testing ? "Testing…" : "Test"}
      </button>
      <button type="button" title="Details" onClick={open} className={`${iconBtn} hover:text-accent-foreground`}>
        <ChevronRight aria-hidden size={14} strokeWidth={2} />
      </button>
    </div>
  );
}

function ProvidersTab({ onAdd }: { onAdd: () => void }) {
  const { connections, loaded } = useConnections();

  return (
    <div className="flex flex-col gap-3">
      {connections.length > 0 && (
        <Card>
          {connections.map((c, i) => (
            <ConnectionRow key={c.id} conn={c} index={i} count={connections.length} />
          ))}
        </Card>
      )}
      {loaded && connections.length === 0 && (
        <div className="py-8 text-center text-[13px] text-muted-foreground">
          No connections yet. Add a provider connection to route models through Ryuzi.
        </div>
      )}
      {loaded && (
        <button
          type="button"
          onClick={onAdd}
          className="flex cursor-pointer items-center gap-3 rounded-xl border border-dashed border-border bg-transparent px-[18px] py-[15px] font-sans text-muted-foreground hover:bg-accent hover:text-accent-foreground"
        >
          <Plus aria-hidden size={16} strokeWidth={2} />
          <span className="text-[13px] font-medium">Add connection</span>
        </button>
      )}
    </div>
  );
}

export function ModelsView() {
  const [tab, setTab] = useState<Tab>("endpoint");
  const [addOpen, setAddOpen] = useState(false);
  const { loaded: endpointLoaded, hydrate: hydrateEndpoint } = useEndpoint();
  const { loaded: connectionsLoaded, hydrate: hydrateConnections } = useConnections();

  useEffect(() => {
    if (!endpointLoaded) void hydrateEndpoint();
  }, [endpointLoaded, hydrateEndpoint]);
  useEffect(() => {
    if (!connectionsLoaded) void hydrateConnections();
  }, [connectionsLoaded, hydrateConnections]);

  return (
    <div className="min-h-0 flex-1 overflow-y-auto px-8 py-7">
      <div className="mx-auto max-w-[860px]">
        <div className="mb-5 flex items-start gap-3">
          <div className="min-w-0 flex-1">
            <h2 className="m-0 mb-1 text-[22px] font-semibold tracking-[-0.02em]">Models</h2>
            <p className="m-0 text-[13px] text-muted-foreground">The local model endpoint and the provider connections that back it.</p>
          </div>
          <Segmented
            options={[
              { id: "endpoint", label: "Endpoint" },
              { id: "connections", label: "Providers" },
            ]}
            value={tab}
            onChange={setTab}
          />
        </div>

        {tab === "endpoint" ? <EndpointTab /> : <ProvidersTab onAdd={() => setAddOpen(true)} />}
      </div>
      <AddConnectionModal open={addOpen} onClose={() => setAddOpen(false)} />
    </div>
  );
}
