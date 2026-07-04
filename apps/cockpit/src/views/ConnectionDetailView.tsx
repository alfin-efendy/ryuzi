import { useEffect, useState } from "react";
import { Trash2 } from "lucide-react";
import { toast } from "sonner";
import { useConnections } from "@/store-connections";
import { useNav } from "@/store-nav";
import { Card, CardHeader, CardRow, CardTitle } from "@/components/common/Card";
import { BackButton, DetailHeader } from "@/components/common/DetailHeader";
import { Chip } from "@/components/common/bits";

const field = "h-9 rounded-md border border-input bg-background px-3 font-sans text-[12.5px] text-foreground";
const modelsField = "min-h-[96px] w-full resize-y rounded-md border border-input bg-background px-3 py-2 font-mono text-xs text-foreground";

export function ConnectionDetailView({ id }: { id: string }) {
  const nav = useNav();
  const { connections, loaded, hydrate, update, remove, test } = useConnections();
  const [label, setLabel] = useState("");
  const [apiKey, setApiKey] = useState("");
  const [baseUrl, setBaseUrl] = useState("");
  const [modelsText, setModelsText] = useState("");
  const [initFor, setInitFor] = useState<string | null>(null);
  const [saving, setSaving] = useState(false);
  const [testing, setTesting] = useState(false);

  useEffect(() => {
    if (!loaded) void hydrate();
  }, [loaded, hydrate]);

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

  return (
    <div className="min-h-0 flex-1 overflow-y-auto px-8 pb-10 pt-[22px]">
      <div className="mx-auto max-w-[860px]">
        <BackButton label="Models" onClick={() => nav.navigate({ kind: "models" })} />

        <DetailHeader
          chip={<Chip initial={conn.initial} color={conn.color} size={44} />}
          title={conn.label || conn.providerName}
          sub={conn.providerName}
        >
          <button
            type="button"
            onClick={() => void runTest()}
            disabled={testing}
            className="flex h-8 shrink-0 cursor-pointer items-center gap-[7px] rounded-md border border-border bg-transparent px-3 font-sans text-[12.5px] font-medium text-foreground hover:bg-accent disabled:opacity-50"
          >
            {testing ? "Testing…" : "Test"}
          </button>
          <button
            type="button"
            title="Remove connection"
            onClick={() => void del()}
            className="flex h-8 w-8 shrink-0 cursor-pointer items-center justify-center rounded-md border border-border bg-transparent text-destructive hover:bg-accent"
          >
            <Trash2 aria-hidden size={13} strokeWidth={2} />
          </button>
        </DetailHeader>

        <Card>
          <CardHeader>
            <CardTitle>Identity</CardTitle>
          </CardHeader>
          <CardRow>
            <span className="w-28 shrink-0 text-[13px] font-medium">Provider</span>
            <span className="flex-1 text-[13px] text-muted-foreground">{conn.providerName}</span>
          </CardRow>
          <CardRow>
            <span className="w-28 shrink-0 text-[13px] font-medium">Label</span>
            <input className={`${field} flex-1`} value={label} onChange={(e) => setLabel(e.target.value)} placeholder={conn.providerName} />
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
            <input
              type="password"
              className={`${field} flex-1`}
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
            <input className={`${field} flex-1`} value={baseUrl} onChange={(e) => setBaseUrl(e.target.value)} placeholder="https://host/v1" />
          </CardRow>
          <div className="px-[18px] pb-3 text-[11.5px] text-muted-foreground">Leave empty for the provider default.</div>
        </Card>

        <Card className="mt-3">
          <CardHeader>
            <CardTitle>Models</CardTitle>
          </CardHeader>
          <div className="px-[18px] py-3">
            <textarea className={modelsField} value={modelsText} onChange={(e) => setModelsText(e.target.value)} placeholder="one model per line" />
            <div className="mt-1.5 text-[11.5px] text-muted-foreground">Leave empty for the provider's default list.</div>
          </div>
        </Card>

        <div className="mt-4 flex justify-end">
          <button
            type="button"
            onClick={() => void save()}
            disabled={saving}
            className="h-9 cursor-pointer rounded-md border-none bg-primary px-4 font-sans text-[13px] font-medium text-primary-foreground hover:opacity-85 disabled:opacity-50"
          >
            {saving ? "Saving…" : "Save"}
          </button>
        </div>
      </div>
    </div>
  );
}
