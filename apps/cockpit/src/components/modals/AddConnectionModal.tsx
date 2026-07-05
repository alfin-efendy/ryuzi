import { useEffect, useMemo, useRef, useState } from "react";
import { Copy, Loader2 } from "lucide-react";
import { useConnections } from "@/store-connections";
import { events, type CatalogEntry } from "@/bindings";
import { Chip } from "@/components/common/bits";
import { Button, FormField, Input, Modal, ModalFooter } from "@ryuzi/ui";

const COMPATIBLE_IDS = ["custom-openai", "custom-anthropic"] as const;

type CompatibleId = (typeof COMPATIBLE_IDS)[number];

function formatLabel(entry: CatalogEntry): string {
  return entry.format === "anthropic" ? "Anthropic-compatible" : "OpenAI-compatible";
}

function cardHint(entry: CatalogEntry): string {
  return entry.format === "anthropic" ? "Messages API format" : "Chat Completions API format";
}

function fallbackEntry(id: CompatibleId): CatalogEntry {
  const anthropic = id === "custom-anthropic";
  return {
    id,
    name: anthropic ? "Custom (Anthropic-compatible)" : "Custom (OpenAI-compatible)",
    color: "#8B8B8B",
    initial: "C",
    category: "api_key",
    format: anthropic ? "anthropic" : "openai",
    requiresBaseUrl: true,
    models: [],
  };
}

function compatibleEntries(catalog: CatalogEntry[]): CatalogEntry[] {
  return COMPATIBLE_IDS.map((id) => catalog.find((entry) => entry.id === id) ?? fallbackEntry(id));
}

export function AddConnectionModal({
  open,
  onClose,
  provider,
}: {
  open: boolean;
  onClose: () => void;
  provider?: string;
}) {
  const { catalog, add, connectOauth, addFree } = useConnections();
  const [selectedId, setSelectedId] = useState<CompatibleId>("custom-openai");
  const [label, setLabel] = useState("");
  const [apiKey, setApiKey] = useState("");
  const [baseUrl, setBaseUrl] = useState("");
  const [saving, setSaving] = useState(false);
  const [oauthWaiting, setOauthWaiting] = useState(false);
  const [oauthAuthorizeUrl, setOauthAuthorizeUrl] = useState("");
  const choices = useMemo(() => compatibleEntries(catalog), [catalog]);
  const fixed = provider ? catalog.find((entry) => entry.id === provider) : null;
  const selected = fixed ?? choices.find((entry) => entry.id === selectedId) ?? choices[0];
  const title = provider ? "Add account" : "Add connection";
  const selectedRef = useRef<CatalogEntry | null>(selected);

  useEffect(() => {
    selectedRef.current = selected;
  }, [selected]);

  useEffect(() => {
    if (!open) return;
    setSelectedId("custom-openai");
    setLabel("");
    setApiKey("");
    setBaseUrl("");
    setSaving(false);
    setOauthWaiting(false);
    setOauthAuthorizeUrl("");
  }, [open, provider]);

  useEffect(() => {
    if (!open) return;
    let active = true;
    let unlisten: (() => void) | null = null;

    void events.oauthAuthorizeUrlMsg.listen((event) => {
      if (!active || selectedRef.current?.id !== event.payload.provider) return;
      setOauthAuthorizeUrl(event.payload.authorizeUrl);
    }).then((stop) => {
      if (active) unlisten = stop;
      else stop();
    });

    return () => {
      active = false;
      unlisten?.();
    };
  }, [open]);

  if (!open) return null;

  const close = () => {
    setLabel("");
    setApiKey("");
    setBaseUrl("");
    setSaving(false);
    setOauthWaiting(false);
    setOauthAuthorizeUrl("");
    onClose();
  };

  const baseUrlMissing = selected?.category === "api_key" && !!selected.requiresBaseUrl && baseUrl.trim().length === 0;
  const canSubmit = !!selected && !saving && !baseUrlMissing;

  const submitApiKey = async () => {
    if (!selected || !canSubmit) return;
    setSaving(true);
    const ok = await add(selected.id, label.trim() || selected.name, apiKey, baseUrl.trim() || null);
    setSaving(false);
    if (ok) close();
  };

  const submitFree = async () => {
    if (!selected || !canSubmit) return;
    setSaving(true);
    const ok = await addFree(selected.id, label.trim() || selected.name);
    setSaving(false);
    if (ok) close();
  };

  const connectBrowser = async () => {
    if (!selected || saving) return;
    const target = selected;
    setSaving(true);
    setOauthWaiting(true);
    setOauthAuthorizeUrl("");
    const ok = await connectOauth(target.id, label.trim() || target.name);
    if (selectedRef.current?.id !== target.id) return;
    setSaving(false);
    if (ok) close();
    else setOauthWaiting(false);
  };

  const copyAuthorizeUrl = () => {
    if (oauthAuthorizeUrl) void navigator.clipboard.writeText(oauthAuthorizeUrl);
  };

  return (
    <Modal onClose={close} width={480}>
      <div className="flex items-center gap-3">
        <Chip initial={selected?.initial ?? "C"} color={selected?.color ?? "#8B8B8B"} size={36} />
        <div className="min-w-0 flex-1">
          <div className="text-[15px] font-semibold tracking-[-0.01em]">{title}</div>
          <div className="text-xs text-muted-foreground">
            {selected ? (provider ? selected.name : "Connect an OpenAI-compatible or Anthropic-compatible endpoint") : "Provider unavailable"}
          </div>
        </div>
      </div>

      {!provider && (
        <div role="radiogroup" aria-label="Connection format" className="mt-4 grid grid-cols-2 gap-2">
          {choices.map((entry) => {
            const checked = entry.id === selectedId;
            return (
              <Button
                key={entry.id}
                role="radio"
                aria-checked={checked}
                aria-label={formatLabel(entry)}
                variant={checked ? "secondary" : "outline"}
                onClick={() => setSelectedId(entry.id as CompatibleId)}
                className="h-auto w-full justify-start gap-[11px] px-3 py-[11px] text-left"
              >
                <Chip initial={entry.initial} color={entry.color} size={32} />
                <span className="min-w-0">
                  <span className="block font-semibold">{formatLabel(entry)}</span>
                  <span className="block overflow-hidden text-ellipsis whitespace-nowrap text-[11px] font-normal text-muted-foreground">
                    {cardHint(entry)}
                  </span>
                </span>
              </Button>
            );
          })}
        </div>
      )}

      {selected?.category === "oauth" ? (
        <>
          <div className="mt-3.5 flex flex-col gap-3">
            <FormField label="Label">
              <Input value={label} onChange={(event) => setLabel(event.target.value)} placeholder={selected.name} />
            </FormField>
          </div>
          {!oauthWaiting ? (
            <Button size="lg" onClick={() => void connectBrowser()} disabled={saving} className="mt-3.5 w-full">
              {saving ? "Opening..." : "Connect with browser"}
            </Button>
          ) : (
            <div className="mt-3.5 flex flex-col gap-3">
              <div className="flex items-center gap-2 rounded-md border border-border px-3 py-2.5 text-[12.5px] text-muted-foreground">
                <Loader2 aria-hidden size={13} strokeWidth={2} className="shrink-0 animate-spin" />
                Waiting for your browser... complete the login, then return here.
              </div>
              {oauthAuthorizeUrl && (
                <FormField label="Login URL">
                  <div className="flex min-w-0 gap-2">
                    <Input
                      readOnly
                      value={oauthAuthorizeUrl}
                      onFocus={(event) => event.currentTarget.select()}
                      className="min-w-0 font-mono text-[11.5px]"
                    />
                    <Button type="button" variant="outline" onClick={copyAuthorizeUrl} className="shrink-0" aria-label="Copy login URL">
                      <Copy aria-hidden size={13} strokeWidth={2} className="size-3.5" />
                      Copy
                    </Button>
                  </div>
                </FormField>
              )}
            </div>
          )}
        </>
      ) : (
        <>
          <div className="mt-3.5 flex flex-col gap-3">
            <FormField label="Label">
              <Input value={label} onChange={(event) => setLabel(event.target.value)} placeholder={selected?.name ?? "Connection"} />
            </FormField>
            {selected?.category === "api_key" && (
              <>
                <FormField label="API key">
                  <Input type="password" value={apiKey} onChange={(event) => setApiKey(event.target.value)} placeholder="sk-..." />
                </FormField>
                <FormField
                  label={
                    selected.requiresBaseUrl ? (
                      "Base URL"
                    ) : (
                      <>
                        Base URL override<span className="font-normal text-muted-foreground"> - optional</span>
                      </>
                    )
                  }
                >
                  <Input value={baseUrl} onChange={(event) => setBaseUrl(event.target.value)} placeholder="https://host/v1" />
                </FormField>
              </>
            )}
          </div>

          <Button
            size="lg"
            onClick={() => void (selected?.category === "free" ? submitFree() : submitApiKey())}
            disabled={!canSubmit}
            className="mt-3.5 w-full"
          >
            {saving ? "Adding..." : title}
          </Button>
        </>
      )}

      <ModalFooter className="mt-4">
        <Button variant="outline" onClick={close}>
          Cancel
        </Button>
      </ModalFooter>
    </Modal>
  );
}
