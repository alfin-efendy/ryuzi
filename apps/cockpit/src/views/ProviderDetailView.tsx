import { useEffect, useMemo, useState } from "react";
import { ArrowDown, ArrowUp, Pencil, Plus, RefreshCw, TestTube2, Trash2 } from "lucide-react";
import { toast } from "sonner";
import { commands, type ConnectionInfo, type ModelRouteStrategy, type SelectableModelInfo, type UsageSeries } from "@/bindings";
import { useConnections } from "@/store-connections";
import { useUsage } from "@/store-usage";
import { useNav } from "@/store-nav";
import { useUi } from "@/store-ui";
import { useModelStatuses } from "@/store-model-statuses";
import { runPool, visibleModels, type ModelTestEntry, type ModelTestStatus } from "@/lib/model-testing";
import {
  Button,
  Combobox,
  SettingsCard as Card,
  SettingsCardHeader as CardHeader,
  SettingsCardHint as CardHint,
  SettingsCardTitle as CardTitle,
  Switch,
} from "@ryuzi/ui";
import { BackButton, DetailHeader } from "@/components/common/DetailHeader";
import { Chip } from "@/components/common/bits";
import { UsageChart } from "@/components/common/UsageChart";
import { AddConnectionModal } from "@/components/modals/AddConnectionModal";
import { ModelCapabilityIcons } from "@/components/ModelCapabilityIcons";
import { useAgent } from "@/store-agent";
import { useStore } from "@/store";
import { AccountQuotaSummary } from "@/components/AccountQuotaSummary";
import { usesDeviceSignin } from "@/components/modals/deviceSignin";
import { RenameAccountModal } from "@/components/modals/RenameAccountModal";
import { ConfirmAccountActionModal, type ConfirmAccountAction } from "@/components/modals/ConfirmAccountActionModal";

function accountLabel(count: number): string {
  return `${count} account${count === 1 ? "" : "s"}`;
}

function modelLabel(count: number, prefix = ""): string {
  return `${count} ${prefix}model${count === 1 ? "" : "s"}`;
}

function strategyText(strategy: ModelRouteStrategy): string {
  return strategy === "round-robin" ? "Round robin" : "By order";
}

export function accountReconnectKind(conn: ConnectionInfo, entry: Parameters<typeof usesDeviceSignin>[0] | undefined) {
  if (conn.authType !== "oauth") return null;
  return entry && usesDeviceSignin(entry) ? "device" : "redirect";
}

export function modelEffortDefaultOptions(metadata: SelectableModelInfo) {
  const inheritedLabel =
    metadata.defaultSource === "variesByTarget" ? "Default: varies by target" : `Default: ${metadata.resolvedDefault ?? "provider"}`;
  return [
    { value: "__model_default__", label: inheritedLabel },
    ...metadata.supported.map((option) => ({ value: option.value, label: option.label, description: option.description ?? undefined })),
  ];
}

export function ModelEffortDefaultCombobox({
  metadata,
  onChange,
}: {
  metadata: SelectableModelInfo;
  onChange: (key: NonNullable<SelectableModelInfo["preferenceKey"]>, effort: string | null) => void;
}) {
  const key = metadata.preferenceKey;
  if (!key) return null;
  const configuredLabel = metadata.supported.find((option) => option.value === metadata.configuredDefault)?.label;
  const inheritedLabel = modelEffortDefaultOptions(metadata)[0].label;
  return (
    <Combobox
      aria-label={`Default effort for ${metadata.displayName}`}
      options={modelEffortDefaultOptions(metadata)}
      value={metadata.configuredDefault ?? "__model_default__"}
      onValueChange={(value) => onChange(key, value === "__model_default__" ? null : value)}
      trigger={
        <Button variant="outline" size="sm" className="w-[180px] justify-start">
          {configuredLabel ? `Default: ${configuredLabel}` : inheritedLabel}
        </Button>
      }
      className="w-[180px]"
    />
  );
}

function aggregateUsage(series: Array<UsageSeries | undefined>): UsageSeries | null {
  const present = series.filter((item): item is UsageSeries => !!item);
  if (present.length === 0) return null;
  const byDay = new Map<string, { day: string; requests: number; inputTokens: number; outputTokens: number }>();
  for (const usage of present) {
    for (const point of usage.days) {
      const current = byDay.get(point.day) ?? { day: point.day, requests: 0, inputTokens: 0, outputTokens: 0 };
      current.requests += point.requests;
      current.inputTokens += point.inputTokens;
      current.outputTokens += point.outputTokens;
      byDay.set(point.day, current);
    }
  }
  return {
    days: Array.from(byDay.values()).sort((a, b) => a.day.localeCompare(b.day)),
    todayRequests: present.reduce((n, item) => n + item.todayRequests, 0),
    todayInputTokens: present.reduce((n, item) => n + item.todayInputTokens, 0),
    todayOutputTokens: present.reduce((n, item) => n + item.todayOutputTokens, 0),
  };
}

function AccountRow({
  conn,
  index,
  count,
  deviceSignin,
  onRename,
  onDelete,
  onResetCredit,
  onDeviceReconnect,
}: {
  conn: ConnectionInfo;
  index: number;
  count: number;
  deviceSignin: boolean;
  onRename: () => void;
  onDelete: (trigger: HTMLButtonElement) => void;
  onResetCredit: (request: { accountName: string; onConfirm: () => Promise<boolean>; trigger: HTMLButtonElement }) => void;
  onDeviceReconnect: () => void;
}) {
  const setEnabled = useConnections((s) => s.setEnabled);
  const move = useConnections((s) => s.move);
  const test = useConnections((s) => s.test);
  const reconnectOauth = useConnections((s) => s.reconnectOauth);
  const [testing, setTesting] = useState(false);
  const [reconnecting, setReconnecting] = useState(false);

  const name = conn.label || conn.providerName;

  const runTest = async () => {
    setTesting(true);
    const result = await test(conn.id);
    setTesting(false);
    if (result) {
      if (result.ok) toast.success(result.message);
      else toast.error(result.message);
    }
  };

  const reconnect = async () => {
    if (deviceSignin) {
      onDeviceReconnect();
      return;
    }
    setReconnecting(true);
    const ok = await reconnectOauth(conn.id);
    setReconnecting(false);
    if (ok) toast.success(`Reconnected ${name}`);
  };

  return (
    <div className="border-b border-border last:border-b-0">
      <div className="flex items-center gap-2 px-[18px] py-3.5">
        <Chip initial={conn.initial} color={conn.color} size={34} />
        <div className="flex shrink-0 items-center gap-1">
          <Button
            variant="ghost"
            size="icon-sm"
            aria-label={`Move ${name} up`}
            title={`Move ${name} up`}
            onClick={() => void move(conn.id, -1)}
            disabled={index === 0}
            className="text-muted-foreground"
          >
            <ArrowUp aria-hidden />
          </Button>
          <Button
            variant="ghost"
            size="icon-sm"
            aria-label={`Move ${name} down`}
            title={`Move ${name} down`}
            onClick={() => void move(conn.id, 1)}
            disabled={index === count - 1}
            className="text-muted-foreground"
          >
            <ArrowDown aria-hidden />
          </Button>
        </div>
        <div className="flex min-w-0 flex-1 items-center gap-1.5">
          <span className="truncate text-sm font-semibold text-foreground">{name}</span>
          <Button variant="ghost" size="icon-sm" aria-label={`Rename ${name}`} onClick={onRename} className="text-muted-foreground">
            <Pencil aria-hidden />
          </Button>
          {conn.needsRelogin && <span className="text-xs text-destructive">Needs re-login</span>}
        </div>
        <Switch on={conn.enabled} onToggle={() => void setEnabled(conn.id, !conn.enabled)} label={`Enabled ${name}`} />
        <Button aria-label={`Test ${name}`} variant="outline" size="sm" onClick={() => void runTest()} disabled={testing}>
          {testing ? "Testing..." : "Test"}
        </Button>
        {conn.authType === "oauth" && (
          <Button aria-label={`Reconnect ${name}`} variant="outline" size="sm" onClick={() => void reconnect()} disabled={reconnecting}>
            {reconnecting ? "Reconnecting…" : "Reconnect"}
          </Button>
        )}
        <Button aria-label={`Delete ${name}`} variant="destructive" size="sm" onClick={(event) => onDelete(event.currentTarget)}>
          <Trash2 aria-hidden data-icon="inline-start" />
          Delete
        </Button>
      </div>
      {conn.quotaCapability && (
        <AccountQuotaSummary connectionId={conn.id} accountName={name} capability={conn.quotaCapability} onRequestReset={onResetCredit} />
      )}
    </div>
  );
}

function StatusBadge({ entry }: { entry: ModelTestEntry }) {
  const look =
    entry.status === "valid"
      ? { symbol: "✓", className: "text-green-500" }
      : entry.status === "invalid"
        ? { symbol: "✗", className: "text-red-500" }
        : { symbol: "?", className: "text-muted-foreground" };
  return (
    <span
      role="img"
      title={entry.message}
      aria-label={`${entry.status}: ${entry.message}`}
      className={`w-4 shrink-0 text-center text-xs font-semibold ${look.className}`}
    >
      {look.symbol}
    </span>
  );
}

function ProviderModelsCard({
  family,
  connections,
  catalogModels,
}: {
  family: string;
  connections: ConnectionInfo[];
  catalogModels: string[];
}) {
  const [results, setResults] = useState<Map<string, ModelTestEntry>>(new Map());
  const [inFlight, setInFlight] = useState<Set<string>>(new Set());
  const [batch, setBatch] = useState<{ done: number; total: number } | null>(null);
  const [refreshing, setRefreshing] = useState(false);
  const [refreshErrors, setRefreshErrors] = useState<Array<{ connectionId: string; message: string }>>([]);
  const hideInvalid = useUi((s) => s.hideInvalidModels);
  const toggleHideInvalid = useUi((s) => s.toggleHideInvalidModels);
  const hydrate = useConnections((s) => s.hydrate);
  const selectableModels = useAgent((s) => s.models);
  const setModelEffortPreference = useStore((s) => s.setModelEffortPreference);
  const refreshModelConfiguration = useStore((s) => s.refreshModelConfiguration);
  const models = useMemo(() => {
    const set = new Set<string>();
    for (const conn of connections) {
      for (const model of conn.models) set.add(model);
    }
    if (set.size === 0) {
      for (const model of catalogModels) set.add(model);
    }
    return Array.from(set).sort((a, b) => a.localeCompare(b));
  }, [catalogModels, connections]);
  const visible = useMemo(() => visibleModels(models, results, hideInvalid), [models, results, hideInvalid]);

  useEffect(() => {
    let active = true;
    void commands.listModelStatuses(family).then((result) => {
      if (!active || result.status !== "ok") return;
      setResults(new Map(result.data.map((row) => [row.model, { status: row.status as ModelTestStatus, message: row.message }])));
    });
    return () => {
      active = false;
    };
  }, [family]);

  const connFor = (model: string) =>
    connections.find((item) => item.enabled && item.models.includes(model)) ?? connections.find((item) => item.models.includes(model));

  const testOne = async (model: string): Promise<ModelTestEntry | null> => {
    const conn = connFor(model);
    if (!conn) return null;
    setInFlight((prev) => new Set(prev).add(model));
    try {
      let entry: ModelTestEntry;
      try {
        const result = await commands.testConnectionModel(conn.id, model);
        entry =
          result.status === "ok"
            ? { status: result.data.status as ModelTestStatus, message: result.data.message }
            : { status: "unknown", message: `Model test failed: ${result.error.message}` };
      } catch (e) {
        entry = { status: "unknown", message: e instanceof Error ? e.message : String(e) };
      }
      setResults((prev) => new Map(prev).set(model, entry));
      // Feed the app-wide status store so every model picker (not just this
      // card) updates live. runTestAll funnels each model through testOne,
      // so batch runs are covered by this single call site.
      useModelStatuses.getState().upsert(family, model, entry.status);
      return entry;
    } finally {
      setInFlight((prev) => {
        const next = new Set(prev);
        next.delete(model);
        return next;
      });
    }
  };

  const runModelTest = async (model: string) => {
    const entry = await testOne(model);
    if (!entry) return;
    if (entry.status === "valid") toast.success(entry.message);
    else toast.error(entry.message);
  };

  const runTestAll = async () => {
    const targets = models.filter((model) => connFor(model));
    if (targets.length === 0) return;
    setBatch({ done: 0, total: targets.length });
    try {
      // Cap of 3: every probe is a real billed inference call.
      await runPool(targets, 3, async (model) => {
        await testOne(model);
        setBatch((prev) => (prev ? { done: prev.done + 1, total: prev.total } : prev));
      });
    } finally {
      setBatch(null);
    }
  };

  const runRefresh = async () => {
    setRefreshing(true);
    setRefreshErrors([]);
    const result = await commands.refreshProviderModels(family);
    setRefreshing(false);
    if (result.status !== "ok") {
      toast.error(`Refresh failed: ${result.error.message}`);
      return;
    }
    const failures = result.data.filter((item) => !item.ok);
    setRefreshErrors(failures.map((item) => ({ connectionId: item.connectionId, message: item.message })));
    if (failures.length === 0) toast.success("Models refreshed");
    await hydrate();
    await refreshModelConfiguration();
  };

  return (
    <Card className="mt-3">
      <CardHeader className="flex-wrap">
        <CardTitle>Models</CardTitle>
        <CardHint>{modelLabel(models.length)}</CardHint>
        <div className="ml-auto flex items-center gap-2">
          <span className="text-xs font-medium text-muted-foreground">Hide invalid</span>
          <Switch on={hideInvalid} onToggle={toggleHideInvalid} label="Hide invalid models" />
          <Button variant="outline" size="sm" onClick={() => void runTestAll()} disabled={batch !== null || connections.length === 0}>
            <TestTube2 aria-hidden size={12} strokeWidth={2} className="size-3" />
            {batch ? `Testing ${batch.done}/${batch.total}` : "Test all"}
          </Button>
          <Button variant="outline" size="sm" onClick={() => void runRefresh()} disabled={refreshing || connections.length === 0}>
            <RefreshCw aria-hidden size={12} strokeWidth={2} className="size-3" />
            {refreshing ? "Refreshing..." : "Refresh models"}
          </Button>
        </div>
      </CardHeader>
      {refreshErrors.map((err) => (
        <div key={err.connectionId} className="border-b border-border px-[18px] py-2 text-xs last:border-b-0" style={{ color: "#EF4444" }}>
          {err.message}
        </div>
      ))}
      {visible.map((model) => {
        const entry = results.get(model);
        const testing = inFlight.has(model);
        const metadata = selectableModels.find(
          (candidate) =>
            candidate.kind === "concrete" &&
            candidate.preferenceKey?.family === family &&
            candidate.preferenceKey.model === model &&
            candidate.supported.length > 0,
        );
        return (
          <div key={model} className="flex min-h-11 items-center gap-2 border-b border-border px-[18px] py-2.5 last:border-b-0">
            <span className="min-w-0 flex-1 truncate font-mono text-xs text-foreground">{model}</span>
            {entry && <StatusBadge entry={entry} />}
            <ModelCapabilityIcons model={model} compact />
            {metadata?.preferenceKey ? (
              <ModelEffortDefaultCombobox metadata={metadata} onChange={(key, effort) => void setModelEffortPreference(key, effort)} />
            ) : null}
            <Button
              variant="outline"
              size="sm"
              onClick={() => void runModelTest(model)}
              disabled={testing || connections.length === 0}
              aria-label={`Test ${model}`}
            >
              {testing ? (
                <RefreshCw aria-hidden size={12} strokeWidth={2} className="size-3 animate-spin" />
              ) : (
                <TestTube2 aria-hidden size={12} strokeWidth={2} className="size-3" />
              )}
              {testing ? "Testing..." : "Test"}
            </Button>
          </div>
        );
      })}
      {visible.length === 0 && models.length > 0 && (
        <div className="px-[18px] py-8 text-center text-[13px] text-muted-foreground">All models hidden by the Hide-invalid filter.</div>
      )}
      {models.length === 0 && <div className="px-[18px] py-8 text-center text-[13px] text-muted-foreground">No models discovered yet.</div>}
    </Card>
  );
}

export function ProviderDetailView({ provider }: { provider: string }) {
  const nav = useNav();
  const { catalog, connections, loaded, hydrate, rename, remove } = useConnections();
  const usageByConnection = useUsage((s) => s.byConnection);
  const loadUsage = useUsage((s) => s.loadConnection);
  const [addOpen, setAddOpen] = useState(false);
  const [accountStrategy, setAccountStrategy] = useState<ModelRouteStrategy>("fallback");
  const [savingStrategy, setSavingStrategy] = useState(false);
  const [renameConnection, setRenameConnection] = useState<ConnectionInfo | null>(null);
  const [confirmAction, setConfirmAction] = useState<ConfirmAccountAction | null>(null);

  useEffect(() => {
    if (!loaded) void hydrate();
  }, [loaded, hydrate]);

  useEffect(() => {
    let active = true;
    void commands.providerAccountRoute(provider).then((result) => {
      if (active && result.status === "ok") setAccountStrategy(result.data.strategy);
    });
    return () => {
      active = false;
    };
  }, [provider]);

  const memberIds = useMemo(
    () => new Set(catalog.filter((entry) => entry.family === provider).map((entry) => entry.id)),
    [catalog, provider],
  );
  const providerConnections = useMemo(
    () => connections.filter((c) => memberIds.has(c.provider) || c.provider === provider),
    [connections, memberIds, provider],
  );
  const providerConnectionIds = providerConnections.map((conn) => conn.id).join("|");

  // biome-ignore lint/correctness/useExhaustiveDependencies: keyed on providerConnectionIds so usage reloads only when the set of connection ids changes, not on every providerConnections re-derive
  useEffect(() => {
    for (const conn of providerConnections) {
      void loadUsage(conn.id);
    }
  }, [providerConnectionIds, providerConnections, loadUsage]);

  const catalogEntry = catalog.find((c) => c.id === provider);
  const fallback = providerConnections[0];
  const name = catalogEntry?.name ?? fallback?.providerName ?? provider;
  const color = catalogEntry?.color ?? fallback?.color ?? "#8B8B8B";
  const initial = catalogEntry?.initial ?? fallback?.initial ?? "?";
  const catalogModels = useMemo(() => {
    const set = new Set<string>();
    for (const entry of catalog) {
      if (entry.family === provider) for (const model of entry.models) set.add(model);
    }
    return Array.from(set);
  }, [catalog, provider]);
  const activeCount = providerConnections.filter((c) => c.enabled).length;
  // biome-ignore lint/correctness/useExhaustiveDependencies: keyed on providerConnectionIds so usage re-aggregates only when the set of connection ids changes
  const usage = useMemo(
    () => aggregateUsage(providerConnections.map((conn) => usageByConnection[conn.id])),
    [providerConnectionIds, providerConnections, usageByConnection],
  );

  const setStrategy = async (strategy: ModelRouteStrategy) => {
    setAccountStrategy(strategy);
    setSavingStrategy(true);
    const result = await commands.setProviderAccountRoute(provider, strategy);
    setSavingStrategy(false);
    if (result.status === "ok") {
      setAccountStrategy(result.data.strategy);
      return;
    }
    toast.error(`Account routing failed: ${result.error.message}`);
  };

  if (loaded && !catalogEntry && providerConnections.length === 0) {
    return (
      <div className="flex flex-1 flex-col items-center justify-center gap-3 text-[13px] text-muted-foreground">
        Provider not found.
        <BackButton label="Models" onClick={() => nav.navigate({ kind: "models" })} />
      </div>
    );
  }

  return (
    <div className="min-h-0 flex-1 overflow-y-auto px-8 pb-10 pt-[22px]">
      <div className="mx-auto max-w-[860px]">
        <BackButton label="Models" onClick={() => nav.navigate({ kind: "models" })} />
        <h2 className="sr-only">{name}</h2>

        <DetailHeader
          chip={<Chip initial={initial} color={color} size={44} />}
          title={name}
          sub={`${accountLabel(providerConnections.length)} · ${modelLabel(catalogModels.length, "catalog ")}`}
        />

        <Card>
          <CardHeader className="flex-wrap">
            <CardTitle>Accounts</CardTitle>
            <CardHint>
              {providerConnections.length > 0 ? `${activeCount} active · ${strategyText(accountStrategy)}` : "No accounts connected"}
            </CardHint>
            <div className="ml-auto flex items-center gap-2">
              <span className="text-xs font-medium text-muted-foreground">Account routing</span>
              <Combobox
                aria-label="Account routing"
                options={[
                  { value: "fallback", label: "By order" },
                  { value: "round-robin", label: "Round robin" },
                ]}
                value={accountStrategy}
                onValueChange={(v) => void setStrategy(v as ModelRouteStrategy)}
                disabled={savingStrategy}
                className="w-[140px]"
              />
              <Button onClick={() => setAddOpen(true)}>
                <Plus aria-hidden data-icon="inline-start" />
                Add account
              </Button>
            </div>
          </CardHeader>
          {providerConnections.map((conn, index) => (
            <AccountRow
              key={conn.id}
              conn={conn}
              index={index}
              count={providerConnections.length}
              deviceSignin={(() => {
                const entry = catalog.find((candidate) => candidate.id === conn.provider);
                return accountReconnectKind(conn, entry) === "device";
              })()}
              onRename={() => setRenameConnection(conn)}
              onDelete={(trigger) => {
                setConfirmAction({
                  kind: "delete",
                  accountName: conn.label || conn.providerName,
                  onConfirm: () => remove(conn.id),
                  trigger,
                });
              }}
              onResetCredit={(request) => setConfirmAction({ kind: "resetCredit", ...request })}
              onDeviceReconnect={() => setAddOpen(true)}
            />
          ))}
          {loaded && providerConnections.length === 0 && (
            <div className="px-[18px] py-8 text-center text-[13px] text-muted-foreground">
              No accounts yet. Add an account for this provider to route models through Ryuzi.
            </div>
          )}
        </Card>

        <Card className="mt-3">
          <CardHeader>
            <CardTitle>Usage</CardTitle>
            <CardHint>Last 14 days across this provider</CardHint>
          </CardHeader>
          <div className="px-[18px] py-3">
            <UsageChart points={usage?.days ?? []} />
            {usage && (
              <div className="mt-2 text-xs text-muted-foreground">
                Today: {usage.todayRequests} req · {(usage.todayInputTokens + usage.todayOutputTokens).toLocaleString()} tokens
              </div>
            )}
          </div>
        </Card>

        <ProviderModelsCard family={provider} connections={providerConnections} catalogModels={catalogModels} />
      </div>
      <AddConnectionModal open={addOpen} onClose={() => setAddOpen(false)} family={provider} />
      <RenameAccountModal
        open={renameConnection !== null}
        connection={renameConnection}
        onClose={() => setRenameConnection(null)}
        onRename={(label) => (renameConnection ? rename(renameConnection.id, label) : Promise.resolve(false))}
      />
      <ConfirmAccountActionModal
        open={confirmAction !== null}
        action={confirmAction}
        onClose={() => {
          setConfirmAction(null);
        }}
      />
    </div>
  );
}
