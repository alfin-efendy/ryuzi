import { useCallback, useEffect, useState } from "react";
import { Trash2 } from "lucide-react";
import { toast } from "sonner";
import { commands, type ProviderQuotaCapability, type ProviderQuotaInfo } from "@/bindings";
import { useConnections } from "@/store-connections";
import { useNav } from "@/store-nav";
import { usesDeviceSignin } from "@/components/modals/deviceSignin";
import {
  Button,
  Input,
  SettingsCard as Card,
  SettingsCardHeader as CardHeader,
  SettingsCardRow as CardRow,
  SettingsCardTitle as CardTitle,
  Switch,
} from "@ryuzi/ui";
import { BackButton, DetailHeader } from "@/components/common/DetailHeader";
import { Chip, Pill } from "@/components/common/bits";
import { ProviderQuotaCard } from "@/components/ProviderQuotaCard";

export function ConnectionDetailView({ id }: { id: string }) {
  const nav = useNav();
  const { connections, catalog, loaded, hydrate, update, remove, test, reconnectOauth } = useConnections();
  const [label, setLabel] = useState("");
  const [apiKey, setApiKey] = useState("");
  const [baseUrl, setBaseUrl] = useState("");
  const [claudeCloaking, setClaudeCloaking] = useState(false);
  const [initFor, setInitFor] = useState<string | null>(null);
  const [saving, setSaving] = useState(false);
  const [testing, setTesting] = useState(false);
  const [reconnecting, setReconnecting] = useState(false);
  const [providerQuota, setProviderQuota] = useState<ProviderQuotaInfo | null>(null);
  const [quotaLoading, setQuotaLoading] = useState(false);
  const [resettingCredit, setResettingCredit] = useState(false);

  useEffect(() => {
    if (!loaded) void hydrate();
  }, [loaded, hydrate]);

  const conn = connections.find((c) => c.id === id);
  const quotaCapability: ProviderQuotaCapability | null = conn?.quotaCapability ?? null;

  const loadProviderQuota = useCallback(async () => {
    if (!conn || !quotaCapability) {
      setProviderQuota(null);
      return;
    }
    setQuotaLoading(true);
    const result = await commands.connectionProviderQuota(conn.id);
    setQuotaLoading(false);
    if (result.status === "ok") {
      setProviderQuota(result.data);
      return;
    }
    toast.error(`Quota failed: ${result.error.message}`);
  }, [conn, quotaCapability]);

  useEffect(() => {
    if (quotaCapability) void loadProviderQuota();
    else setProviderQuota(null);
  }, [quotaCapability, loadProviderQuota]);

  // Seed the form fields once per connection — later hydrations (e.g. after
  // Save) shouldn't clobber an in-progress edit.
  useEffect(() => {
    if (conn && initFor !== conn.id) {
      setLabel(conn.label);
      setApiKey("");
      setBaseUrl(conn.baseUrl ?? "");
      setClaudeCloaking(conn.claudeCloaking);
      setInitFor(conn.id);
    }
  }, [conn, initFor]);

  if (loaded && !conn) {
    return (
      <div className="flex flex-1 flex-col items-center justify-center gap-3 text-[13px] text-muted-foreground">
        Connection not found.
        <BackButton label="Models" onClick={() => nav.navigate({ kind: "models" })} />
      </div>
    );
  }
  if (!conn) return null;

  const providerFamily = catalog.find((entry) => entry.id === conn.provider)?.family ?? conn.provider;

  const save = async () => {
    setSaving(true);
    await update(id, {
      label,
      enabled: conn.enabled,
      apiKey: apiKey || null,
      baseUrl: baseUrl || null,
      models: conn.models,
      claudeCloaking: conn.provider === "anthropic-oauth" ? claudeCloaking : null,
    });
    setApiKey("");
    setSaving(false);
  };

  const runTest = async () => {
    setTesting(true);
    const result = await test(id);
    setTesting(false);
    if (result) {
      if (result.ok) toast.success(result.message);
      else toast.error(result.message);
    }
  };

  const del = async () => {
    if (!window.confirm(`Remove ${conn.label || conn.providerName}? This cannot be undone.`)) return;
    await remove(id);
    nav.navigate({ kind: "providerDetail", provider: providerFamily });
  };

  const needsRelogin = conn.authType === "oauth" && conn.needsRelogin;
  // Kiro authenticates via the AWS SSO-OIDC device-code flow, not the
  // redirect+PKCE flow `reconnectOauth` drives — there's no loopback
  // callback to re-run in place. Point the user back at "Add connection"
  // (sign in again or re-import from the Kiro IDE) instead of trying, and
  // failing, to reuse the redirect-based reconnect.
  const isKiro = conn.provider === "kiro";
  const catalogEntry = catalog.find((entry) => entry.id === conn.provider);
  // Kiro (device flow) AND device-grant providers (qwen, github-copilot) sign in
  // via a device code, not the redirect+PKCE flow reconnectOauth drives.
  const deviceSignin = catalogEntry ? usesDeviceSignin(catalogEntry) : isKiro;

  const reconnect = async () => {
    setReconnecting(true);
    const ok = await reconnectOauth(conn.id);
    setReconnecting(false);
    if (ok) toast.success(`Reconnected ${conn.providerName}`);
  };

  const resetCodexCredit = async () => {
    if (!window.confirm("Spend one Codex reset credit now? This cannot be undone.")) return;
    setResettingCredit(true);
    const result = await commands.resetCodexCredit(conn.id);
    setResettingCredit(false);
    if (result.status === "ok") {
      if (result.data.reset) toast.success("Codex reset credit applied.");
      else toast.error(result.data.message ?? "No Codex reset credits available.");
      await loadProviderQuota();
      return;
    }
    toast.error(`Reset credit failed: ${result.error.message}`);
  };

  const reconnectDevice = () => {
    nav.navigate({ kind: "models" });
    toast(
      isKiro
        ? "Reconnect Kiro from Add connection — sign in again or import from the Kiro IDE."
        : `Reconnect ${conn.providerName} from Add connection — sign in again.`,
    );
  };

  return (
    <div className="min-h-0 flex-1 overflow-y-auto px-8 pb-10 pt-[22px]">
      <div className="mx-auto max-w-[860px]">
        <BackButton
          label={catalog.find((entry) => entry.id === providerFamily)?.name ?? conn.providerName}
          onClick={() => nav.navigate({ kind: "providerDetail", provider: providerFamily })}
        />

        <DetailHeader
          chip={<Chip initial={conn.initial} color={conn.color} size={44} />}
          title={label || conn.providerName}
          titleNode={
            <Input
              aria-label="Connection label"
              className="h-9 w-full max-w-[420px] min-w-0 border-transparent bg-transparent px-0 text-xl font-semibold shadow-none hover:border-border focus-visible:border-ring"
              value={label}
              onChange={(event) => setLabel(event.target.value)}
              placeholder={conn.providerName}
            />
          }
          titleExtra={needsRelogin ? <Pill variant="warn">Needs re-login</Pill> : undefined}
          sub={conn.providerName}
        >
          {needsRelogin && deviceSignin && (
            <Button onClick={reconnectDevice} className="shrink-0">
              Reconnect via Add connection
            </Button>
          )}
          {needsRelogin && !deviceSignin && (
            <Button onClick={() => void reconnect()} disabled={reconnecting} className="shrink-0">
              {reconnecting ? "Reconnecting…" : "Reconnect"}
            </Button>
          )}
          <Button variant="outline" onClick={() => void runTest()} disabled={testing} className="shrink-0">
            {testing ? "Testing…" : "Test"}
          </Button>
          <Button
            variant="outline"
            size="icon"
            title="Remove connection"
            onClick={() => void del()}
            className="shrink-0 text-destructive hover:text-destructive"
          >
            <Trash2 aria-hidden size={13} strokeWidth={2} className="size-[13px]" />
          </Button>
        </DetailHeader>

        {quotaCapability && (
          <ProviderQuotaCard
            capability={quotaCapability}
            quota={providerQuota}
            loading={quotaLoading}
            resetting={resettingCredit}
            onRefresh={() => void loadProviderQuota()}
            onResetCredit={() => void resetCodexCredit()}
          />
        )}

        <Card className="mt-3">
          <CardHeader>
            <CardTitle>Credential</CardTitle>
          </CardHeader>
          <CardRow>
            <span className="w-28 shrink-0 text-[13px] font-medium">Current key</span>
            <span className="flex-1 font-mono text-xs text-muted-foreground">{conn.keyMasked ?? "no key set"}</span>
          </CardRow>
          <CardRow>
            <span className="w-28 shrink-0 text-[13px] font-medium">Replace key</span>
            <Input
              type="password"
              className="flex-1"
              value={apiKey}
              onChange={(e) => setApiKey(e.target.value)}
              placeholder="Leave empty to keep the current key"
            />
          </CardRow>
        </Card>

        <Card className="mt-3">
          <CardHeader>
            <CardTitle>Base URL</CardTitle>
          </CardHeader>
          <CardRow>
            <Input className="flex-1" value={baseUrl} onChange={(e) => setBaseUrl(e.target.value)} placeholder="https://host/v1" />
          </CardRow>
          <div className="px-[18px] pb-3 text-[11.5px] text-muted-foreground">Leave empty for the provider default.</div>
        </Card>

        {conn.provider === "anthropic-oauth" && (
          <Card className="mt-3">
            <CardHeader>
              <CardTitle>Claude Code</CardTitle>
            </CardHeader>
            <CardRow>
              <div className="min-w-0 flex-1">
                <div className="text-[13px] font-medium">Cloaking</div>
                <div className="mt-0.5 text-[11.5px] text-muted-foreground">
                  Send Claude Code-style headers, metadata, billing block, and tool names.
                </div>
              </div>
              <Switch on={claudeCloaking} onToggle={() => setClaudeCloaking((value) => !value)} label="Claude Code cloaking" />
            </CardRow>
          </Card>
        )}

        <div className="mt-4 flex justify-end">
          <Button size="lg" onClick={() => void save()} disabled={saving}>
            {saving ? "Saving…" : "Save"}
          </Button>
        </div>
      </div>
    </div>
  );
}
