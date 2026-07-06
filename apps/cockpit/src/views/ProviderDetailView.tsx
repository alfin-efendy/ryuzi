import { useEffect, useMemo, useState } from "react";
import { ArrowDown, ArrowUp, ChevronRight, Plus, TestTube2 } from "lucide-react";
import { toast } from "sonner";
import { commands, type ConnectionInfo, type ModelRouteStrategy, type UsageSeries } from "@/bindings";
import { useConnections } from "@/store-connections";
import { useUsage } from "@/store-usage";
import { useNav } from "@/store-nav";
import {
  Button,
  NativeSelect,
  SettingsCard as Card,
  SettingsCardHeader as CardHeader,
  SettingsCardHint as CardHint,
  SettingsCardTitle as CardTitle,
  Switch,
} from "@ryuzi/ui";
import { BackButton, DetailHeader } from "@/components/common/DetailHeader";
import { Chip, Pill } from "@/components/common/bits";
import { UsageChart } from "@/components/common/UsageChart";
import { AddConnectionModal } from "@/components/modals/AddConnectionModal";
import { ModelCapabilityIcons } from "@/components/ModelCapabilityIcons";

function accountLabel(count: number): string {
  return `${count} account${count === 1 ? "" : "s"}`;
}

function modelLabel(count: number, prefix = ""): string {
  return `${count} ${prefix}model${count === 1 ? "" : "s"}`;
}

function strategyText(strategy: ModelRouteStrategy): string {
  return strategy === "round-robin" ? "Round robin" : "By order";
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

function AccountRow({ conn, index, count }: { conn: ConnectionInfo; index: number; count: number }) {
  const nav = useNav();
  const update = useConnections((s) => s.update);
  const move = useConnections((s) => s.move);
  const test = useConnections((s) => s.test);
  const [testing, setTesting] = useState(false);

  const open = () => nav.navigate({ kind: "connectionDetail", id: conn.id });
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

  return (
    <div className="flex items-center gap-2 border-b border-border px-[18px] py-3.5 last:border-b-0">
      <Chip initial={conn.initial} color={conn.color} size={34} onClick={open} />
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
          <ArrowUp aria-hidden size={13} strokeWidth={2} className="size-[13px]" />
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
          <ArrowDown aria-hidden size={13} strokeWidth={2} className="size-[13px]" />
        </Button>
      </div>
      <Button
        variant="ghost"
        onClick={open}
        className="h-auto min-w-0 flex-1 flex-col items-start gap-0 whitespace-normal p-0 text-left font-normal"
      >
        <span className="block text-sm font-semibold text-foreground">{name}</span>
        <span className="block text-xs text-muted-foreground">
          {conn.authType === "oauth" ? "Subscription" : conn.authType === "free" ? "Free" : "API key"} · {conn.keyMasked ?? "no key"} ·{" "}
          {modelLabel(conn.models.length)}
          {conn.needsRelogin ? " · needs re-login" : ""}
        </span>
      </Button>
      <Switch
        on={conn.enabled}
        onToggle={() =>
          void update(conn.id, {
            label: conn.label,
            enabled: !conn.enabled,
            apiKey: null,
            baseUrl: conn.baseUrl,
            models: conn.models,
            claudeCloaking: conn.provider === "anthropic-oauth" ? conn.claudeCloaking : null,
          })
        }
        label="Enabled"
      />
      <Button variant="outline" size="sm" onClick={() => void runTest()} disabled={testing}>
        {testing ? "Testing..." : "Test"}
      </Button>
      <Button variant="ghost" size="icon-sm" title="Details" onClick={open} className="text-muted-foreground">
        <ChevronRight aria-hidden size={14} strokeWidth={2} className="size-3.5" />
      </Button>
    </div>
  );
}

function ProviderModelsCard({ connections, catalogModels }: { connections: ConnectionInfo[]; catalogModels: string[] }) {
  const [testingModel, setTestingModel] = useState<string | null>(null);
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

  const runModelTest = async (model: string) => {
    const conn =
      connections.find((item) => item.enabled && item.models.includes(model)) ?? connections.find((item) => item.models.includes(model));
    if (!conn) return;
    setTestingModel(model);
    const result = await commands.testConnectionModel(conn.id, model);
    setTestingModel(null);
    if (result.status === "ok") {
      if (result.data.ok) toast.success(result.data.message);
      else toast.error(result.data.message);
      return;
    }
    toast.error(`Model test failed: ${result.error.message}`);
  };

  return (
    <Card className="mt-3">
      <CardHeader>
        <CardTitle>Models</CardTitle>
        <CardHint>{modelLabel(models.length)}</CardHint>
      </CardHeader>
      {models.map((model) => (
        <div key={model} className="flex min-h-11 items-center gap-2 border-b border-border px-[18px] py-2.5 last:border-b-0">
          <span className="min-w-0 flex-1 truncate font-mono text-xs text-foreground">{model}</span>
          <ModelCapabilityIcons model={model} compact />
          <Button
            variant="outline"
            size="sm"
            onClick={() => void runModelTest(model)}
            disabled={testingModel === model || connections.length === 0}
            aria-label={`Test ${model}`}
          >
            <TestTube2 aria-hidden size={12} strokeWidth={2} className="size-3" />
            {testingModel === model ? "Testing..." : "Test"}
          </Button>
        </div>
      ))}
      {models.length === 0 && <div className="px-[18px] py-8 text-center text-[13px] text-muted-foreground">No models discovered yet.</div>}
    </Card>
  );
}

export function ProviderDetailView({ provider }: { provider: string }) {
  const nav = useNav();
  const { catalog, connections, loaded, hydrate } = useConnections();
  const usageByConnection = useUsage((s) => s.byConnection);
  const loadUsage = useUsage((s) => s.loadConnection);
  const [addOpen, setAddOpen] = useState(false);
  const [accountStrategy, setAccountStrategy] = useState<ModelRouteStrategy>("fallback");
  const [savingStrategy, setSavingStrategy] = useState(false);

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
          titleExtra={activeCount > 0 ? <Pill variant="primary">{activeCount} active</Pill> : undefined}
        />

        <Card>
          <CardHeader className="flex-wrap">
            <CardTitle>Accounts</CardTitle>
            <CardHint>
              {providerConnections.length > 0 ? `${activeCount} active · ${strategyText(accountStrategy)}` : "No accounts connected"}
            </CardHint>
            <div className="ml-auto flex items-center gap-2">
              <span className="text-xs font-medium text-muted-foreground">Account routing</span>
              <NativeSelect
                aria-label="Account routing"
                value={accountStrategy}
                onChange={(event) => void setStrategy(event.target.value as ModelRouteStrategy)}
                disabled={savingStrategy}
                className="w-[140px]"
              >
                <option value="fallback">By order</option>
                <option value="round-robin">Round robin</option>
              </NativeSelect>
              <Button onClick={() => setAddOpen(true)}>
                <Plus aria-hidden size={14} strokeWidth={2} className="size-3.5" />
                Add account
              </Button>
            </div>
          </CardHeader>
          {providerConnections.map((conn, index) => (
            <AccountRow key={conn.id} conn={conn} index={index} count={providerConnections.length} />
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

        <ProviderModelsCard connections={providerConnections} catalogModels={catalogModels} />
      </div>
      <AddConnectionModal open={addOpen} onClose={() => setAddOpen(false)} family={provider} />
    </div>
  );
}
