import { useEffect, useMemo, useRef, useState } from "react";
import { Loader2 } from "lucide-react";
import { toast } from "sonner";
import { useConnections } from "@/store-connections";
import { events, type CatalogEntry, type DeviceFlowInfo } from "@/bindings";
import { Chip } from "@/components/common/bits";
import { Button, ChoiceCard, FormField, Input, Modal, ModalBody, ModalFooter, ModalHeader, RadioGroup } from "@ryuzi/ui";
import {
  DEVICE_SIGNIN_ACTION,
  KIRO_DEVICE_CODE_HINT,
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
type SignInFlow = "device" | "oauth" | "apiKey" | "free";

const SUBSCRIPTION_LABELS: Record<string, string> = {
  "anthropic-oauth": "Claude subscription",
  "openai-oauth": "ChatGPT subscription",
};

const BASE_URL_PLACEHOLDERS: Record<string, string> = {
  "cloudflare-ai": "https://api.cloudflare.com/client/v4/accounts/{account_id}/ai/v1",
};

function signInFlow(entry: CatalogEntry): SignInFlow {
  if (usesDeviceSignin(entry)) return "device";
  if (entry.category === "oauth") return "oauth";
  if (entry.category === "free") return "free";
  return "apiKey";
}

function authMethodLabel(entry: CatalogEntry): string {
  switch (signInFlow(entry)) {
    case "device":
      return "Device sign-in";
    case "oauth":
      return SUBSCRIPTION_LABELS[entry.id] ?? "Subscription";
    case "free":
      return "Free tier";
    case "apiKey":
      return "API key";
  }
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
  const selectedRef = useRef<CatalogEntry | null>(selected);
  const operationRef = useRef(0);

  useEffect(() => {
    selectedRef.current = selected;
  }, [selected]);

  // biome-ignore lint/correctness/useExhaustiveDependencies: changing family resets the modal and invalidates its pending operation.
  useEffect(() => {
    operationRef.current += 1;
    if (open) {
      setSelectedId(null);
      setLabel("");
      setApiKey("");
      setBaseUrl("");
      setSaving(false);
      setOauthWaiting(false);
      setOauthAuthorizeUrl("");
      setDeviceStep("form");
      setDeviceInfo(null);
    }

    return () => {
      operationRef.current += 1;
    };
  }, [open, family]);

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

  const beginOperation = () => {
    operationRef.current += 1;
    return operationRef.current;
  };
  const operationIsCurrent = (operation: number) => operationRef.current === operation;

  const close = () => {
    operationRef.current += 1;
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

  const flow = selected ? signInFlow(selected) : null;
  const baseUrlMissing = flow === "apiKey" && !!selected?.requiresBaseUrl && baseUrl.trim().length === 0;
  const canSubmit = !!selected && !saving && !baseUrlMissing;
  const shortCommitBusy = saving && (flow === "apiKey" || flow === "free");

  const selectMethod = (id: string) => {
    if (shortCommitBusy || id === selected?.id) return;
    operationRef.current += 1;
    setSelectedId(id);
    setSaving(false);
    setOauthWaiting(false);
    setOauthAuthorizeUrl("");
    setDeviceStep("form");
    setDeviceInfo(null);
  };

  const submitApiKey = async () => {
    if (!selected || !canSubmit) return;
    const target = selected;
    const operation = beginOperation();
    setSaving(true);
    const ok = await add(target.id, label.trim() || target.name, apiKey, baseUrl.trim() || null);
    if (!operationIsCurrent(operation)) return;
    setSaving(false);
    if (ok) close();
  };

  const submitFree = async () => {
    if (!selected || !canSubmit) return;
    const target = selected;
    const operation = beginOperation();
    setSaving(true);
    const ok = await addFree(target.id, label.trim() || target.name);
    if (!operationIsCurrent(operation)) return;
    setSaving(false);
    if (ok) close();
  };

  const connectBrowser = async () => {
    if (!selected || saving) return;
    const target = selected;
    const operation = beginOperation();
    setSaving(true);
    setOauthWaiting(true);
    setOauthAuthorizeUrl("");
    const ok = await connectOauth(target.id, label.trim() || target.name);
    if (!operationIsCurrent(operation)) return;
    setSaving(false);
    if (ok) close();
    else setOauthWaiting(false);
  };

  const copyAuthorizeUrl = () => {
    if (oauthAuthorizeUrl) void navigator.clipboard.writeText(oauthAuthorizeUrl);
  };

  const startDevice = async () => {
    if (!selected || saving) return;
    const target = selected;
    const operation = beginOperation();
    setSaving(true);
    const info = target.usesDeviceGrant ? await startDeviceFlow(target.id) : await startKiroDevice();
    if (!operationIsCurrent(operation)) return;
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
    if (!operationIsCurrent(operation)) return;
    setSaving(false);
    if (ok) close();
    else {
      setDeviceInfo(null);
      setDeviceStep("form");
    }
  };

  const importFromIde = async () => {
    if (!selected || saving) return;
    const target = selected;
    const operation = beginOperation();
    setSaving(true);
    const ok = await importKiro(label.trim() || target.name);
    if (!operationIsCurrent(operation)) return;
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
    <Modal onClose={close} width={480} busy={shortCommitBusy}>
      <ModalHeader
        leading={<Chip initial={selected?.initial ?? "C"} color={selected?.color ?? "#8B8B8B"} size={36} />}
        title="Add account"
        description={selected?.name ?? "Provider unavailable"}
      />
      <ModalBody>
        {members.length > 1 && (
          <RadioGroup
            aria-label="Sign-in method"
            value={selected?.id ?? ""}
            onValueChange={selectMethod}
            disabled={shortCommitBusy}
            className="grid-cols-2"
          >
            {members.map((entry) => (
              <ChoiceCard
                key={entry.id}
                value={entry.id}
                title={authMethodLabel(entry)}
                description={entry.name}
                leading={<Chip initial={entry.initial} color={entry.color} size={32} />}
              />
            ))}
          </RadioGroup>
        )}

        {selected?.riskNotice && (
          <p className="mt-3 rounded-md border border-border px-3 py-2 text-[11.5px] text-amber-500">{PROVIDER_RISK_NOTICE}</p>
        )}

        {flow === "oauth" && selected && (
          <div className="mt-3.5 flex flex-col gap-3">
            <FormField label="Label">
              <Input value={label} onChange={(event) => setLabel(event.target.value)} placeholder={selected.name} />
            </FormField>
            {oauthWaiting && (
              <>
                <div className="flex items-center gap-2 rounded-md border border-border px-3 py-2.5 text-[12.5px] text-muted-foreground">
                  <Loader2 aria-hidden className="shrink-0 animate-spin" />
                  Waiting for your browser... complete the login, then return here.
                </div>
                {oauthAuthorizeUrl && (
                  <FormField label="Login URL">
                    <Input
                      readOnly
                      value={oauthAuthorizeUrl}
                      onFocus={(event) => event.currentTarget.select()}
                      className="font-mono text-[11.5px]"
                    />
                  </FormField>
                )}
              </>
            )}
          </div>
        )}

        {flow === "device" && selected && deviceStep === "form" && (
          <div className="mt-3.5 flex flex-col gap-3">
            <FormField label="Label">
              <Input value={label} onChange={(event) => setLabel(event.target.value)} placeholder={selected.name} />
            </FormField>
            <p className="text-[11.5px] text-muted-foreground">
              {selected.id === "kiro"
                ? KIRO_SIGNIN_SUBTITLE
                : (PROVIDER_DEVICE_SUBTITLE[selected.id] ?? "Sign in with your provider account.")}
            </p>
            {selected.id === "kiro" && <p className="text-[11.5px] text-muted-foreground">{KIRO_IMPORT_HINT}</p>}
          </div>
        )}

        {flow === "device" && deviceStep === "waiting" && deviceInfo && (
          <div className="mt-3.5 flex flex-col items-center gap-3 rounded-md border border-border px-4 py-4 text-center">
            <p className="text-[12.5px] text-muted-foreground">{KIRO_DEVICE_CODE_HINT}</p>
            <span className="font-mono text-lg font-semibold tracking-[0.08em]">{deviceInfo.userCode}</span>
            <div className="flex items-center gap-2 text-[12px] text-muted-foreground">
              <Loader2 aria-hidden className="shrink-0 animate-spin" />
              {KIRO_WAITING_HINT}
            </div>
            {deviceInfo.verificationUri && (
              <p className="break-all text-[11px] text-muted-foreground">
                or visit <span className="font-mono">{deviceInfo.verificationUri}</span>
              </p>
            )}
          </div>
        )}

        {(flow === "apiKey" || flow === "free") && selected && (
          <div className="mt-3.5 flex flex-col gap-3">
            <FormField label="Label">
              <Input value={label} onChange={(event) => setLabel(event.target.value)} placeholder={selected.name} />
            </FormField>
            {flow === "apiKey" && (
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
                    placeholder={BASE_URL_PLACEHOLDERS[selected.id] ?? "https://host/v1"}
                  />
                </FormField>
              </>
            )}
          </div>
        )}
      </ModalBody>

      <ModalFooter>
        {oauthWaiting && oauthAuthorizeUrl && (
          <Button variant="outline" onClick={copyAuthorizeUrl}>
            Copy login URL
          </Button>
        )}
        {deviceStep === "waiting" && deviceInfo && (
          <Button variant="outline" onClick={copyDeviceCode}>
            Copy code
          </Button>
        )}
        {flow === "device" && deviceStep === "form" && selected?.id === "kiro" && (
          <Button variant="outline" disabled={saving} onClick={() => void importFromIde()}>
            Import from Kiro IDE
          </Button>
        )}
        <div className="flex-1" />
        <Button variant="outline" disabled={shortCommitBusy} onClick={close}>
          Cancel
        </Button>
        {flow === "oauth" && !oauthWaiting && (
          <Button disabled={saving} onClick={() => void connectBrowser()}>
            {saving ? "Opening..." : "Connect with browser"}
          </Button>
        )}
        {flow === "device" && deviceStep === "form" && (
          <Button disabled={saving} onClick={() => void startDevice()}>
            {saving ? "Opening..." : selected?.id === "kiro" ? KIRO_SIGNIN_ACTION : DEVICE_SIGNIN_ACTION}
          </Button>
        )}
        {(flow === "apiKey" || flow === "free") && (
          <Button disabled={!canSubmit} onClick={() => void (flow === "free" ? submitFree() : submitApiKey())}>
            {saving ? "Adding..." : "Add account"}
          </Button>
        )}
      </ModalFooter>
    </Modal>
  );
}
