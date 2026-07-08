import { useEffect, useMemo, useRef, useState } from "react";
import { Copy, Loader2 } from "lucide-react";
import { toast } from "sonner";
import { useConnections } from "@/store-connections";
import { events, type CatalogEntry, type DeviceFlowInfo } from "@/bindings";
import { CategoryBadge, Chip } from "@/components/common/bits";
import { Button, FormField, Input, Modal, ModalFooter } from "@ryuzi/ui";
import {
  DEVICE_SIGNIN_ACTION,
  KIRO_DEVICE_CODE_HINT,
  KIRO_IMPORT_ACTION,
  KIRO_IMPORT_HINT,
  KIRO_IMPORT_SUCCESS,
  KIRO_SIGNIN_ACTION,
  KIRO_SIGNIN_SUBTITLE,
  KIRO_WAITING_HINT,
  PROVIDER_DEVICE_SUBTITLE,
  PROVIDER_RISK_NOTICE,
} from "@/constants";
import { usesDeviceSignin } from "./deviceSignin";

type DeviceStep = "form" | "waiting";

const SUBSCRIPTION_LABELS: Record<string, string> = {
  "anthropic-oauth": "Claude subscription",
  "openai-oauth": "ChatGPT subscription",
};

const BASE_URL_PLACEHOLDERS: Record<string, string> = {
  "cloudflare-ai": "https://api.cloudflare.com/client/v4/accounts/{account_id}/ai/v1",
};

function authMethodLabel(entry: CatalogEntry): string {
  if (entry.category === "oauth") return SUBSCRIPTION_LABELS[entry.id] ?? "Subscription";
  if (entry.category === "device") return "Device sign-in";
  if (entry.category === "free") return "Free tier";
  return "API key";
}

export function AddConnectionModal({ open, onClose, family }: { open: boolean; onClose: () => void; family: string }) {
  const { catalog, add, connectOauth, addFree, startKiroDevice, awaitKiroDevice, importKiro, startDeviceFlow, awaitDeviceFlow } =
    useConnections();
  const members = useMemo(() => catalog.filter((entry) => entry.family === family), [catalog, family]);
  const [selectedId, setSelectedId] = useState<string | null>(null);
  const [label, setLabel] = useState("");
  const [apiKey, setApiKey] = useState("");
  const [baseUrl, setBaseUrl] = useState("");
  const [saving, setSaving] = useState(false);
  const [oauthWaiting, setOauthWaiting] = useState(false);
  const [oauthAuthorizeUrl, setOauthAuthorizeUrl] = useState("");
  const [deviceStep, setDeviceStep] = useState<DeviceStep>("form");
  const [deviceInfo, setDeviceInfo] = useState<DeviceFlowInfo | null>(null);
  const selected =
    members.find((entry) => entry.id === selectedId) ?? members.find((entry) => entry.category === "api_key") ?? members[0] ?? null;
  const title = "Add account";
  const selectedRef = useRef<CatalogEntry | null>(selected);

  useEffect(() => {
    selectedRef.current = selected;
  }, [selected]);

  useEffect(() => {
    if (!open) return;
    setSelectedId(null);
    setLabel("");
    setApiKey("");
    setBaseUrl("");
    setSaving(false);
    setOauthWaiting(false);
    setOauthAuthorizeUrl("");
    setDeviceStep("form");
    setDeviceInfo(null);
  }, [open]);

  useEffect(() => {
    if (!open) return;
    let active = true;
    let unlisten: (() => void) | null = null;

    void events.oauthAuthorizeUrlMsg
      .listen((event) => {
        if (!active || selectedRef.current?.id !== event.payload.provider) return;
        setOauthAuthorizeUrl(event.payload.authorizeUrl);
      })
      .then((stop) => {
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
    setDeviceStep("form");
    setDeviceInfo(null);
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

  // Kiro (the "device" category) signs in via AWS SSO-OIDC device-code flow;
  // RFC 8628 device-grant providers (qwen, github-copilot) use the generic
  // device flow commands instead. Kiro also supports starting a fresh sign-in
  // or importing an existing sign-in from a Kiro IDE on this machine. The
  // `selectedRef` guard drops results that resolve after the user has
  // navigated to a different provider.
  const startDevice = async () => {
    if (!selected || saving) return;
    const target = selected;
    setSaving(true);
    const info = target.usesDeviceGrant ? await startDeviceFlow(target.id) : await startKiroDevice();
    if (selectedRef.current?.id !== target.id) return;
    if (!info) {
      setSaving(false);
      return;
    }
    setDeviceInfo(info);
    setDeviceStep("waiting");
    const signinLabel = label.trim() || target.name;
    const ok = target.usesDeviceGrant
      ? await awaitDeviceFlow(target.id, signinLabel, info.flowId)
      : await awaitKiroDevice(signinLabel, info.flowId);
    if (selectedRef.current?.id !== target.id) return;
    setSaving(false);
    if (ok) close();
    else setDeviceStep("form");
  };

  const importFromIde = async () => {
    if (!selected || saving) return;
    const target = selected;
    setSaving(true);
    const ok = await importKiro(label.trim() || target.name);
    if (selectedRef.current?.id !== target.id) return;
    setSaving(false);
    if (ok) {
      toast.success(KIRO_IMPORT_SUCCESS);
      close();
    }
  };

  const copyDeviceCode = () => {
    if (!deviceInfo) return;
    void navigator.clipboard.writeText(deviceInfo.userCode);
    toast.success("Copied");
  };

  return (
    <Modal onClose={close} width={480}>
      <div className="flex items-center gap-3">
        <Chip initial={selected?.initial ?? "C"} color={selected?.color ?? "#8B8B8B"} size={36} />
        <div className="min-w-0 flex-1">
          <div className="text-[15px] font-semibold tracking-[-0.01em]">{title}</div>
          <div className="text-xs text-muted-foreground">{selected ? selected.name : "Provider unavailable"}</div>
        </div>
      </div>

      {members.length > 1 && (
        <div role="radiogroup" aria-label="Sign-in method" className="mt-4 grid grid-cols-2 gap-2">
          {members.map((entry) => {
            const checked = entry.id === selected?.id;
            return (
              <Button
                key={entry.id}
                role="radio"
                aria-checked={checked}
                aria-label={authMethodLabel(entry)}
                variant={checked ? "secondary" : "outline"}
                onClick={() => {
                  // Switching sign-in method mid-flight (e.g. while an OAuth
                  // connect is still waiting) must clear the in-flight state,
                  // otherwise `saving`/`oauthWaiting` stay latched and the newly
                  // chosen form is dead until the modal is reopened.
                  if (entry.id === selected?.id) return;
                  setSelectedId(entry.id);
                  setSaving(false);
                  setOauthWaiting(false);
                  setOauthAuthorizeUrl("");
                  setDeviceStep("form");
                  setDeviceInfo(null);
                }}
                className="h-auto w-full justify-start gap-[11px] px-3 py-[11px] text-left"
              >
                <Chip initial={entry.initial} color={entry.color} size={32} />
                <span className="min-w-0">
                  <span className="flex items-center gap-1.5 font-semibold">
                    {authMethodLabel(entry)}
                    <CategoryBadge category={entry.freeTier ? "free_tier" : entry.category} />
                  </span>
                  <span className="block overflow-hidden text-ellipsis whitespace-nowrap text-[11px] font-normal text-muted-foreground">
                    {entry.name}
                  </span>
                </span>
              </Button>
            );
          })}
        </div>
      )}

      {selected?.riskNotice && (
        <p className="mt-3 rounded-md border border-border px-3 py-2 text-[11.5px]" style={{ color: "#F59E0B" }}>
          {PROVIDER_RISK_NOTICE}
        </p>
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
      ) : selected && usesDeviceSignin(selected) ? (
        <>
          {deviceStep === "form" && (
            <>
              <div className="mt-3.5 flex flex-col gap-3">
                <FormField label="Label">
                  <Input value={label} onChange={(event) => setLabel(event.target.value)} placeholder={selected.name} />
                </FormField>
              </div>
              <p className="mt-2 text-[11.5px] text-muted-foreground">
                {selected.id === "kiro"
                  ? KIRO_SIGNIN_SUBTITLE
                  : (PROVIDER_DEVICE_SUBTITLE[selected.id] ?? "Sign in with your provider account.")}
              </p>
              <Button size="lg" onClick={() => void startDevice()} disabled={saving} className="mt-2 w-full">
                {saving ? "Opening..." : selected.id === "kiro" ? KIRO_SIGNIN_ACTION : DEVICE_SIGNIN_ACTION}
              </Button>
              {selected.id === "kiro" && (
                <>
                  <Button size="lg" variant="outline" onClick={() => void importFromIde()} disabled={saving} className="mt-2 w-full">
                    {saving ? "Importing..." : KIRO_IMPORT_ACTION}
                  </Button>
                  <p className="mt-2 text-[11.5px] text-muted-foreground">{KIRO_IMPORT_HINT}</p>
                </>
              )}
            </>
          )}

          {deviceStep === "waiting" && deviceInfo && (
            <div className="mt-3.5 flex flex-col items-center gap-3 rounded-md border border-border px-4 py-4 text-center">
              <p className="text-[12.5px] text-muted-foreground">{KIRO_DEVICE_CODE_HINT}</p>
              <div className="flex items-center gap-1.5">
                <span className="font-mono text-lg font-semibold tracking-[0.08em]">{deviceInfo.userCode}</span>
                <Button variant="ghost" size="icon-sm" title="Copy code" onClick={copyDeviceCode} className="text-muted-foreground">
                  <Copy aria-hidden size={13} strokeWidth={2} className="size-[13px]" />
                </Button>
              </div>
              <div className="flex items-center gap-2 text-[12px] text-muted-foreground">
                <Loader2 aria-hidden size={13} strokeWidth={2} className="shrink-0 animate-spin" />
                {KIRO_WAITING_HINT}
              </div>
              {deviceInfo.verificationUri && (
                <p className="text-[11px] text-muted-foreground break-all">
                  or visit <span className="font-mono">{deviceInfo.verificationUri}</span>
                </p>
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
                  <Input
                    value={baseUrl}
                    onChange={(event) => setBaseUrl(event.target.value)}
                    placeholder={BASE_URL_PLACEHOLDERS[selected?.id ?? ""] ?? "https://host/v1"}
                  />
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
