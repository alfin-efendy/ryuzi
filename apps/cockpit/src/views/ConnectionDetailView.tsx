import { useEffect, useState } from "react";
import { Trash2 } from "lucide-react";
import { toast } from "sonner";
import { useConnections } from "@/store-connections";
import { useUsage } from "@/store-usage";
import { useNav } from "@/store-nav";
import {
  Button,
  Input,
  SettingsCard as Card,
  SettingsCardHeader as CardHeader,
  SettingsCardHint as CardHint,
  SettingsCardRow as CardRow,
  SettingsCardTitle as CardTitle,
  Textarea,
} from "@ryuzi/ui";
import { BackButton, DetailHeader } from "@/components/common/DetailHeader";
import { Chip, Pill } from "@/components/common/bits";
import { UsageChart } from "@/components/common/UsageChart";

export function ConnectionDetailView({ id }: { id: string }) {
  const nav = useNav();
  const { connections, loaded, hydrate, update, remove, test, reconnectOauth } = useConnections();
  const usage = useUsage((s) => s.byConnection[id]);
  const loadUsage = useUsage((s) => s.loadConnection);
  const [label, setLabel] = useState("");
  const [apiKey, setApiKey] = useState("");
  const [baseUrl, setBaseUrl] = useState("");
  const [modelsText, setModelsText] = useState("");
  const [initFor, setInitFor] = useState<string | null>(null);
  const [saving, setSaving] = useState(false);
  const [testing, setTesting] = useState(false);
  const [reconnecting, setReconnecting] = useState(false);

  useEffect(() => {
    if (!loaded) void hydrate();
  }, [loaded, hydrate]);

  useEffect(() => {
    void loadUsage(id);
  }, [id, loadUsage]);

  const conn = connections.find((c) => c.id === id);

  // Seed the form fields once per connection — later hydrations (e.g. after
  // Save) shouldn't clobber an in-progress edit.
  useEffect(() => {
    if (conn && initFor !== conn.id) {
      setLabel(conn.label);
      setApiKey("");
      setBaseUrl(conn.baseUrl ?? "");
      setModelsText(conn.models.join("\n"));
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

  const save = async () => {
    setSaving(true);
    await update(id, {
      label,
      enabled: conn.enabled,
      apiKey: apiKey || null,
      baseUrl: baseUrl || null,
      models: modelsText
        .split("\n")
        .map((s) => s.trim())
        .filter(Boolean),
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
    nav.navigate({ kind: "models" });
  };

  const needsRelogin = conn.authType === "oauth" && conn.needsRelogin;
  // Kiro authenticates via the AWS SSO-OIDC device-code flow, not the
  // redirect+PKCE flow `reconnectOauth` drives — there's no loopback
  // callback to re-run in place. Point the user back at "Add connection"
  // (sign in again or re-import from the Kiro IDE) instead of trying, and
  // failing, to reuse the redirect-based reconnect.
  const isKiro = conn.provider === "kiro";

  const reconnect = async () => {
    setReconnecting(true);
    const ok = await reconnectOauth(conn.id);
    setReconnecting(false);
    if (ok) toast.success(`Reconnected ${conn.providerName}`);
  };

  const reconnectKiro = () => {
    nav.navigate({ kind: "models" });
    toast("Reconnect Kiro from Add connection — sign in again or import from the Kiro IDE.");
  };

  return (
    <div className="min-h-0 flex-1 overflow-y-auto px-8 pb-10 pt-[22px]">
      <div className="mx-auto max-w-[860px]">
        <BackButton label="Models" onClick={() => nav.navigate({ kind: "models" })} />

        <DetailHeader
          chip={<Chip initial={conn.initial} color={conn.color} size={44} />}
          title={conn.label || conn.providerName}
          titleExtra={needsRelogin ? <Pill variant="warn">Needs re-login</Pill> : undefined}
          sub={conn.providerName}
        >
          {needsRelogin && isKiro && (
            <Button onClick={reconnectKiro} className="shrink-0">
              Reconnect via Add connection
            </Button>
          )}
          {needsRelogin && !isKiro && (
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

        <Card>
          <CardHeader>
            <CardTitle>Usage</CardTitle>
            <CardHint>Last 14 days of traffic through this connection</CardHint>
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

        <Card className="mt-3">
          <CardHeader>
            <CardTitle>Identity</CardTitle>
          </CardHeader>
          <CardRow>
            <span className="w-28 shrink-0 text-[13px] font-medium">Provider</span>
            <span className="flex-1 text-[13px] text-muted-foreground">{conn.providerName}</span>
          </CardRow>
          <CardRow>
            <span className="w-28 shrink-0 text-[13px] font-medium">Label</span>
            <Input className="flex-1" value={label} onChange={(e) => setLabel(e.target.value)} placeholder={conn.providerName} />
          </CardRow>
        </Card>

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

        <Card className="mt-3">
          <CardHeader>
            <CardTitle>Models</CardTitle>
          </CardHeader>
          <div className="px-[18px] py-3">
            <Textarea
              className="min-h-[96px] resize-y font-mono text-xs"
              value={modelsText}
              onChange={(e) => setModelsText(e.target.value)}
              placeholder="one model per line"
            />
            <div className="mt-1.5 text-[11.5px] text-muted-foreground">Leave empty for the provider's default list.</div>
          </div>
        </Card>

        <div className="mt-4 flex justify-end">
          <Button size="lg" onClick={() => void save()} disabled={saving}>
            {saving ? "Saving…" : "Save"}
          </Button>
        </div>
      </div>
    </div>
  );
}
