import { useEffect, useMemo, useState } from "react";
import { AlertTriangle, ArrowDown, ArrowUp, ChevronRight, Copy, Plus, Trash2 } from "lucide-react";
import { toast } from "sonner";
import { useEndpoint } from "@/store-endpoint";
import { useConnections } from "@/store-connections";
import { useModelRoutes } from "@/store-model-routes";
import { useUsage } from "@/store-usage";
import { useNav } from "@/store-nav";
import type { CatalogEntry, ConnectionInfo, ModelRouteInfo, ModelRouteStrategy } from "@/bindings";
import {
  Button,
  Input,
  NativeSelect,
  Segmented,
  SettingsCard as Card,
  SettingsCardHeader as CardHeader,
  SettingsCardHint as CardHint,
  SettingsCardRow as CardRow,
  SettingsCardTitle as CardTitle,
  Switch,
} from "@ryuzi/ui";
import { Chip, Pill, StatusDot } from "@/components/common/bits";
import { ModelCapabilityIcons } from "@/components/ModelCapabilityIcons";
import { KEYCHAIN_FILE_FALLBACK_WARNING, KEYCHAIN_UNAVAILABLE_WARNING } from "@/constants";

type Tab = "providers" | "route" | "endpoint";

type ProviderRowInfo = {
  id: string;
  name: string;
  color: string;
  initial: string;
  category: string;
  accounts: ConnectionInfo[];
  catalogModels: number;
  modelCount: number;
};

type TargetOption = {
  key: string;
  connectionId: string;
  model: string;
  providerName: string;
  accountLabel: string;
  enabled: boolean;
};

function accountLabel(count: number): string {
  return count === 0 ? "No accounts" : `${count} account${count === 1 ? "" : "s"}`;
}

function modelLabel(count: number, catalog = false): string {
  return `${count} ${catalog ? "catalog " : ""}model${count === 1 ? "" : "s"}`;
}

function buildProviderRows(catalog: CatalogEntry[], connections: ConnectionInfo[]): ProviderRowInfo[] {
  const rows = new Map<string, ProviderRowInfo>();
  const familyByProvider = new Map(catalog.map((entry) => [entry.id, entry.family]));
  const catalogModelsByFamily = new Map<string, Set<string>>();
  for (const entry of catalog) {
    const models = catalogModelsByFamily.get(entry.family) ?? new Set<string>();
    for (const model of entry.models) models.add(model);
    catalogModelsByFamily.set(entry.family, models);
    if (!rows.has(entry.family)) {
      const head = catalog.find((c) => c.id === entry.family) ?? entry;
      rows.set(entry.family, {
        id: entry.family,
        name: head.name,
        color: head.color,
        initial: head.initial,
        category: head.category,
        accounts: [],
        catalogModels: 0,
        modelCount: 0,
      });
    }
  }
  for (const [family, models] of catalogModelsByFamily) {
    const row = rows.get(family);
    if (row) {
      row.catalogModels = models.size;
      row.modelCount = models.size;
    }
  }
  for (const conn of connections) {
    const family = familyByProvider.get(conn.provider) ?? conn.provider;
    const existing =
      rows.get(family) ??
      ({
        id: family,
        name: conn.providerName,
        color: conn.color,
        initial: conn.initial,
        category: conn.authType,
        accounts: [],
        catalogModels: 0,
        modelCount: 0,
      } satisfies ProviderRowInfo);
    existing.accounts.push(conn);
    const models = new Set(existing.accounts.flatMap((account) => account.models));
    existing.modelCount = models.size || existing.catalogModels;
    rows.set(family, existing);
  }
  return Array.from(rows.values()).sort((a, b) => {
    if (a.accounts.length === 0 && b.accounts.length > 0) return 1;
    if (a.accounts.length > 0 && b.accounts.length === 0) return -1;
    return a.name.localeCompare(b.name);
  });
}

// Matches the warning-banner convention used elsewhere (e.g. RuntimeDetailView's
// endpoint/no-models banners): a bordered row tinted amber for a mild warning,
// red for a stronger one.
const WARN = "#F59E0B";
const DANGER = "#EF4444";

function EndpointTab() {
  const { status, keys, start, stop, setConfig, createKey, revokeKey } = useEndpoint();
  const endpointUsage = useUsage((s) => s.endpoint);
  const loadEndpointUsage = useUsage((s) => s.loadEndpoint);
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

  useEffect(() => {
    void loadEndpointUsage();
  }, [loadEndpointUsage]);

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

  const keychainStatus = status?.keychainStatus;

  return (
    <div className="flex flex-col gap-3">
      {keychainStatus && keychainStatus !== "ok" && (
        <div
          className="flex items-start gap-2 rounded-md border px-3 py-2 text-[12px]"
          style={{
            borderColor: keychainStatus === "unavailable" ? DANGER : WARN,
            color: keychainStatus === "unavailable" ? DANGER : WARN,
          }}
        >
          <AlertTriangle aria-hidden size={14} strokeWidth={2} className="mt-px shrink-0" />
          <span>{keychainStatus === "unavailable" ? KEYCHAIN_UNAVAILABLE_WARNING : KEYCHAIN_FILE_FALLBACK_WARNING}</span>
        </div>
      )}
      <Card>
        <div className="flex items-center gap-3 border-b border-border px-[18px] py-3.5">
          <StatusDot color={status?.running ? "#22C55E" : "var(--muted-foreground)"} pulse={!!status?.running} size={8} />
          <div className="min-w-0 flex-1">
            <div className="text-sm font-semibold text-foreground">{status?.running ? `Running on ${status.baseUrl}` : "Stopped"}</div>
            <div className="mt-0.5 text-xs text-muted-foreground">Local OpenAI-compatible endpoint for external tools.</div>
          </div>
          <Button variant="outline" onClick={() => void toggle()} disabled={busy || !status}>
            {status?.running ? "Stop" : "Start"}
          </Button>
        </div>
        <div className="flex items-center gap-2 px-[18px] py-3">
          <span className="min-w-0 flex-1 truncate font-mono text-xs text-muted-foreground">{status?.baseUrl ?? "—"}</span>
          <Button variant="ghost" size="icon-sm" title="Copy base URL" onClick={copyBaseUrl} className="text-muted-foreground">
            <Copy aria-hidden size={13} strokeWidth={2} className="size-[13px]" />
          </Button>
        </div>
      </Card>

      <Card>
        <CardHeader>
          <CardTitle>Settings</CardTitle>
        </CardHeader>
        <CardRow>
          <span className="w-28 shrink-0 text-[13px] font-medium">Port</span>
          <Input type="number" className="w-28" value={port} onChange={(e) => setPort(e.target.value)} />
        </CardRow>
        <CardRow>
          <span className="flex-1 text-[13px] font-medium">Start automatically with Cockpit</span>
          <Switch on={autostart} onToggle={() => setAutostart(!autostart)} label="Start automatically with Cockpit" />
        </CardRow>
        <div className="flex justify-end px-[18px] py-3">
          <Button onClick={() => void saveConfig()} disabled={savingConfig}>
            {savingConfig ? "Saving…" : "Save"}
          </Button>
        </div>
      </Card>

      <Card>
        <CardHeader>
          <CardTitle>API keys</CardTitle>
          <CardHint>
            Required by external tools calling the local endpoint
            {endpointUsage ? ` · ${endpointUsage.days.reduce((n, d) => n + d.requests, 0)} requests (14d)` : ""}
          </CardHint>
        </CardHeader>
        {keys.map((k) => (
          <div key={k.id} className="flex items-center gap-3 border-b border-border px-[18px] py-3 last:border-b-0">
            <div className="min-w-0 flex-1">
              <div className="text-[13px] font-semibold">{k.name}</div>
              <div className="mt-1 flex items-center gap-1.5">
                <span className="truncate font-mono text-xs text-muted-foreground">{k.key}</span>
                <Button
                  variant="ghost"
                  size="icon-sm"
                  title="Copy key"
                  onClick={() => {
                    void navigator.clipboard.writeText(k.key);
                    toast.success("Copied");
                  }}
                  className="text-muted-foreground"
                >
                  <Copy aria-hidden size={12} strokeWidth={2} className="size-3" />
                </Button>
              </div>
              <div className="mt-1 text-[11px] text-muted-foreground">
                Created {new Date(k.createdAt).toLocaleDateString()} · Last used{" "}
                {k.lastUsedAt ? new Date(k.lastUsedAt).toLocaleDateString() : "never"}
              </div>
            </div>
            <Button variant="destructive" size="sm" onClick={() => void doRevoke(k.id, k.name)}>
              Revoke
            </Button>
          </div>
        ))}
        {keys.length === 0 && (
          <div className="px-[18px] py-3.5 text-[12.5px] text-muted-foreground">No API keys yet — create one for external tools.</div>
        )}
        <div className="flex items-center gap-2 px-[18px] py-3">
          <Input className="flex-1" value={keyName} onChange={(e) => setKeyName(e.target.value)} placeholder="Key name (e.g. VS Code)" />
          <Button size="lg" onClick={() => void submitKey()} disabled={!keyName.trim() || creatingKey}>
            {creatingKey ? "Creating…" : "New key"}
          </Button>
        </div>
      </Card>
    </div>
  );
}

function ProviderRow({ row }: { row: ProviderRowInfo }) {
  const nav = useNav();
  const open = () => nav.navigate({ kind: "providerDetail", provider: row.id });
  const activeCount = row.accounts.filter((account) => account.enabled).length;
  const modelText = modelLabel(row.modelCount, row.accounts.length === 0);

  return (
    <Button
      variant="ghost"
      aria-label={`${row.name} ${accountLabel(row.accounts.length)} ${modelText}`}
      onClick={open}
      className="h-auto w-full justify-start gap-3 rounded-none border-b border-border px-[18px] py-3.5 text-left last:border-b-0"
    >
      <Chip initial={row.initial} color={row.color} size={34} />
      <span className="min-w-0 flex-1">
        <span className="flex items-center gap-1.5 text-sm font-semibold text-foreground">
          {row.name}
          {activeCount > 0 && <Pill variant="primary">{activeCount} active</Pill>}
        </span>
        <span className="block text-xs font-normal text-muted-foreground">
          {accountLabel(row.accounts.length)} · {modelText}
          {row.accounts.length === 0 ? ` · ${row.category.replace("_", " ")}` : ""}
        </span>
      </span>
      <ChevronRight aria-hidden size={14} strokeWidth={2} className="size-3.5 text-muted-foreground" />
    </Button>
  );
}

function ProvidersTab() {
  const { catalog, connections, loaded } = useConnections();
  const rows = useMemo(() => buildProviderRows(catalog, connections), [catalog, connections]);

  return (
    <div className="flex flex-col gap-3">
      {rows.length > 0 && (
        <Card>
          {rows.map((row) => (
            <ProviderRow key={row.id} row={row} />
          ))}
        </Card>
      )}
      {loaded && rows.length === 0 && (
        <div className="py-8 text-center text-[13px] text-muted-foreground">No providers in the catalog yet.</div>
      )}
    </div>
  );
}

function strategyLabel(strategy: ModelRouteStrategy): string {
  return strategy === "round-robin" ? "Round robin" : "By order";
}

function routeTargetOptions(connections: ConnectionInfo[]): TargetOption[] {
  return connections.flatMap((conn) =>
    conn.models.map((model) => ({
      key: `${conn.id}::${model}`,
      connectionId: conn.id,
      model,
      providerName: conn.providerName,
      accountLabel: conn.label || conn.providerName,
      enabled: conn.enabled,
    })),
  );
}

// TODO(Task 14): ModelRouteTarget.provider is meant to be a family id, not a
// connection id — these targets still carry a raw connectionId in that slot
// pending the Route tab's per-model-target redesign.
function newRoute(targets: TargetOption[]): ModelRouteInfo {
  const first = targets[0];
  return {
    id: "",
    name: "",
    enabled: true,
    strategy: "fallback",
    targets: first ? [{ provider: first.connectionId, model: first.model }] : [],
    createdAt: 0,
    updatedAt: 0,
  };
}

function targetKey(target: { provider: string; model: string }): string {
  return `${target.provider}::${target.model}`;
}

function RouteForm({
  value,
  targetOptions,
  saving,
  onCancel,
  onSave,
}: {
  value: ModelRouteInfo;
  targetOptions: TargetOption[];
  saving: boolean;
  onCancel: () => void;
  onSave: (route: ModelRouteInfo) => void;
}) {
  const [draft, setDraft] = useState(value);

  useEffect(() => {
    setDraft(value);
  }, [value]);

  const setTarget = (index: number, key: string) => {
    const option = targetOptions.find((target) => target.key === key);
    if (!option) return;
    setDraft((current) => ({
      ...current,
      // TODO(Task 14): see newRoute — `provider` is a connectionId for now.
      targets: current.targets.map((target, i) => (i === index ? { provider: option.connectionId, model: option.model } : target)),
    }));
  };

  const addTarget = () => {
    const option = targetOptions[0];
    if (!option) return;
    setDraft((current) => ({
      ...current,
      // TODO(Task 14): see newRoute — `provider` is a connectionId for now.
      targets: [...current.targets, { provider: option.connectionId, model: option.model }],
    }));
  };

  const removeTarget = (index: number) => {
    setDraft((current) => ({ ...current, targets: current.targets.filter((_, i) => i !== index) }));
  };
  const moveTarget = (index: number, dir: -1 | 1) => {
    setDraft((current) => {
      const nextIndex = index + dir;
      if (nextIndex < 0 || nextIndex >= current.targets.length) return current;
      const targets = [...current.targets];
      [targets[index], targets[nextIndex]] = [targets[nextIndex], targets[index]];
      return { ...current, targets };
    });
  };

  const canSave = draft.name.trim().length > 0 && draft.targets.length > 0 && targetOptions.length > 0;

  return (
    <Card>
      <CardHeader>
        <CardTitle>{draft.id ? "Edit route" : "New route"}</CardTitle>
        <CardHint>Expose a combo-style model id backed by ordered targets</CardHint>
      </CardHeader>
      <CardRow>
        <span className="w-24 shrink-0 text-[13px] font-medium">Model id</span>
        <Input
          value={draft.name}
          onChange={(event) => setDraft((current) => ({ ...current, name: event.target.value }))}
          placeholder="smart"
          className="flex-1 font-mono"
        />
      </CardRow>
      <CardRow>
        <span className="w-24 shrink-0 text-[13px] font-medium">Strategy</span>
        <NativeSelect
          value={draft.strategy}
          onChange={(event) => setDraft((current) => ({ ...current, strategy: event.target.value as ModelRouteStrategy }))}
          className="max-w-[180px]"
        >
          <option value="fallback">By order</option>
          <option value="round-robin">Round robin</option>
        </NativeSelect>
        <span className="min-w-0 flex-1 text-xs text-muted-foreground">Fallback and capability auto-switch are automatic.</span>
      </CardRow>
      <div className="border-b border-border px-[18px] py-3">
        <div className="mb-2 text-[13px] font-medium">Targets</div>
        <div className="flex flex-col gap-2">
          {draft.targets.map((target, index) => (
            <div key={`${index}-${targetKey(target)}`} className="flex items-center gap-2">
              <NativeSelect value={targetKey(target)} onChange={(event) => setTarget(index, event.target.value)} className="min-w-0 flex-1">
                {targetOptions.map((option) => (
                  <option key={option.key} value={option.key}>
                    {option.providerName} / {option.model} ({option.accountLabel}
                    {option.enabled ? "" : ", disabled"})
                  </option>
                ))}
              </NativeSelect>
              <Button
                variant="ghost"
                size="icon-sm"
                title="Move target up"
                onClick={() => moveTarget(index, -1)}
                disabled={index === 0}
                className="text-muted-foreground"
              >
                <ArrowUp aria-hidden size={13} strokeWidth={2} className="size-[13px]" />
              </Button>
              <Button
                variant="ghost"
                size="icon-sm"
                title="Move target down"
                onClick={() => moveTarget(index, 1)}
                disabled={index === draft.targets.length - 1}
                className="text-muted-foreground"
              >
                <ArrowDown aria-hidden size={13} strokeWidth={2} className="size-[13px]" />
              </Button>
              <Button
                variant="ghost"
                size="icon-sm"
                title="Remove target"
                onClick={() => removeTarget(index)}
                disabled={draft.targets.length === 1}
                className="text-muted-foreground"
              >
                <Trash2 aria-hidden size={13} strokeWidth={2} className="size-[13px]" />
              </Button>
            </div>
          ))}
        </div>
        <Button variant="outline" size="sm" onClick={addTarget} disabled={targetOptions.length === 0} className="mt-2">
          <Plus aria-hidden size={13} strokeWidth={2} className="size-3.5" />
          Add target
        </Button>
      </div>
      <div className="flex justify-end gap-2 px-[18px] py-3">
        <Button variant="ghost" onClick={onCancel} className="text-muted-foreground">
          Cancel
        </Button>
        <Button onClick={() => onSave(draft)} disabled={!canSave || saving}>
          {saving ? "Saving…" : "Save route"}
        </Button>
      </div>
    </Card>
  );
}

// TODO(Task 14): see newRoute — `target.provider` is a connectionId for now.
function RouteTargetPill({ target, connections }: { target: { provider: string; model: string }; connections: ConnectionInfo[] }) {
  const conn = connections.find((c) => c.id === target.provider);
  return (
    <span className="inline-flex min-w-0 max-w-full items-center gap-1 rounded-md bg-muted px-2 py-1 font-mono text-[11.5px] text-muted-foreground">
      <span className="truncate">
        {conn?.providerName ?? "Missing"} / {target.model}
      </span>
      {conn && conn.label !== conn.providerName && <span className="truncate text-[10.5px] opacity-75">({conn.label})</span>}
      <ModelCapabilityIcons model={target.model} compact />
    </span>
  );
}

function RouteCard({
  route,
  connections,
  onEdit,
  onDelete,
}: {
  route: ModelRouteInfo;
  connections: ConnectionInfo[];
  onEdit: () => void;
  onDelete: () => void;
}) {
  const copyName = () => {
    void navigator.clipboard.writeText(route.name);
    toast.success("Copied");
  };

  return (
    <Card>
      <div className="flex items-start gap-3 px-[18px] py-3.5">
        <Chip initial="R" color="#0EA5E9" size={34} mono />
        <div className="min-w-0 flex-1">
          <div className="flex min-w-0 flex-wrap items-center gap-2">
            <span className="truncate font-mono text-sm font-semibold text-foreground">{route.name}</span>
            <Pill variant={route.enabled ? "primary" : "secondary"}>{route.enabled ? "Enabled" : "Disabled"}</Pill>
            <span className="text-xs text-muted-foreground">{strategyLabel(route.strategy)}</span>
          </div>
          <div className="mt-2 flex min-w-0 flex-wrap gap-1.5">
            {route.targets.map((target, index) => (
              <RouteTargetPill key={`${route.id}-${index}-${targetKey(target)}`} target={target} connections={connections} />
            ))}
          </div>
        </div>
        <Button variant="ghost" size="icon-sm" title="Copy model id" onClick={copyName} className="text-muted-foreground">
          <Copy aria-hidden size={13} strokeWidth={2} className="size-[13px]" />
        </Button>
        <Button variant="outline" size="sm" onClick={onEdit}>
          Edit
        </Button>
        <Button variant="ghost" size="icon-sm" title="Delete route" onClick={onDelete} className="text-destructive hover:text-destructive">
          <Trash2 aria-hidden size={13} strokeWidth={2} className="size-[13px]" />
        </Button>
      </div>
    </Card>
  );
}

function RouteTab() {
  const { routes, loaded, hydrate, save, remove } = useModelRoutes();
  const connections = useConnections((s) => s.connections);
  const [editing, setEditing] = useState<ModelRouteInfo | null>(null);
  const [saving, setSaving] = useState(false);
  const targets = useMemo(() => routeTargetOptions(connections), [connections]);

  useEffect(() => {
    if (!loaded) void hydrate();
  }, [loaded, hydrate]);

  const beginNew = () => setEditing(newRoute(targets));
  const saveRoute = async (route: ModelRouteInfo) => {
    setSaving(true);
    const ok = await save(route);
    setSaving(false);
    if (ok) setEditing(null);
  };
  const deleteRoute = async (route: ModelRouteInfo) => {
    if (!window.confirm(`Delete route "${route.name}"?`)) return;
    await remove(route.id);
  };

  return (
    <div className="flex flex-col gap-3">
      {editing ? (
        <RouteForm
          value={editing}
          targetOptions={targets}
          saving={saving}
          onCancel={() => setEditing(null)}
          onSave={(route) => void saveRoute(route)}
        />
      ) : (
        <div className="flex justify-end">
          <Button onClick={beginNew} disabled={targets.length === 0}>
            <Plus aria-hidden size={14} strokeWidth={2} className="size-3.5" />
            New route
          </Button>
        </div>
      )}
      {routes.map((route) => (
        <RouteCard
          key={route.id}
          route={route}
          connections={connections}
          onEdit={() => setEditing(route)}
          onDelete={() => void deleteRoute(route)}
        />
      ))}
      {loaded && routes.length === 0 && !editing && (
        <div className="py-8 text-center text-[13px] text-muted-foreground">
          No routes yet. Create a route alias to expose a combo-style model.
        </div>
      )}
    </div>
  );
}

export function ModelsView() {
  const [tab, setTab] = useState<Tab>("providers");
  const { loaded: endpointLoaded, hydrate: hydrateEndpoint } = useEndpoint();
  const { loaded: connectionsLoaded, hydrate: hydrateConnections } = useConnections();
  const { loaded: routesLoaded, hydrate: hydrateRoutes } = useModelRoutes();

  useEffect(() => {
    if (!endpointLoaded) void hydrateEndpoint();
  }, [endpointLoaded, hydrateEndpoint]);
  useEffect(() => {
    if (!connectionsLoaded) void hydrateConnections();
  }, [connectionsLoaded, hydrateConnections]);
  useEffect(() => {
    if (!routesLoaded) void hydrateRoutes();
  }, [routesLoaded, hydrateRoutes]);

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
              { id: "providers", label: "Providers" },
              { id: "route", label: "Route" },
              { id: "endpoint", label: "Endpoint" },
            ]}
            value={tab}
            onChange={setTab}
          />
        </div>

        {tab === "providers" && <ProvidersTab />}
        {tab === "route" && <RouteTab />}
        {tab === "endpoint" && <EndpointTab />}
      </div>
    </div>
  );
}
