import { openUrl } from "@tauri-apps/plugin-opener";
import { Check, CircleAlert, ExternalLink } from "lucide-react";
import { useCallback, useEffect, useState } from "react";
import { toast } from "sonner";
import { Button, FormField, Input, Modal, ModalFooter } from "@ryuzi/ui";
import { commands, type PluginDetail, type PluginInstallBeginResult } from "@/bindings";
import { IconChip, StatusDot } from "@/components/common/bits";
import { pluginIcon as iconFor } from "@/lib/plugin-icons";

type WizardStep = "checking" | "tokenInput" | "manualClientId" | "waitingOauth" | "settings" | "done";

// "settings" renders only when the manifest declares [[settings]]; every
// terminal path funnels through here so the skip rule lives in one place.
function settingsOrDone(detail: PluginDetail | null): WizardStep {
  return (detail?.settings.length ?? 0) > 0 ? "settings" : "done";
}

// Guided install for catalog plugins. One begin_plugin_install call decides
// the path (env var, token, OAuth browser flow, manual client id) via its
// structured result fields — the wizard never branches on message text.
export function InstallWizardModal({
  pluginId,
  pluginName,
  pluginIcon,
  onClose,
}: {
  pluginId: string;
  pluginName: string;
  pluginIcon?: string | null;
  onClose: () => void;
}) {
  const [step, setStep] = useState<WizardStep>("checking");
  const [detail, setDetail] = useState<PluginDetail | null>(null);
  const [begin, setBegin] = useState<PluginInstallBeginResult | null>(null);
  const [checkError, setCheckError] = useState<string | null>(null);
  const [tokenValue, setTokenValue] = useState("");
  const [clientId, setClientId] = useState("");
  const [busy, setBusy] = useState(false);

  const Icon = iconFor(pluginIcon ?? null);

  const close = () => {
    onClose();
  };

  const submitToken = async () => {
    const key = detail?.auth?.setting;
    if (!key || tokenValue.trim().length === 0 || busy) return;
    setBusy(true);
    const res = await commands.setPluginSetting(key, tokenValue.trim());
    setBusy(false);
    if (res.status === "error") {
      toast.error(res.error.message);
      return;
    }
    setStep(settingsOrDone(detail));
  };

  const submitClientId = async () => {
    if (clientId.trim().length === 0 || busy) return;
    setBusy(true);
    const saved = await commands.setPluginOauthClientId(pluginId, clientId.trim());
    if (saved.status === "error") {
      setBusy(false);
      toast.error(saved.error.message);
      return;
    }
    if (begin?.oauthExternal) {
      // The child server brokers the actual sign-in at first use — no
      // browser flow from Cockpit for external-OAuth plugins.
      setBusy(false);
      setStep(settingsOrDone(detail));
      return;
    }
    // Re-begin: the client id is on the row now, DCR is permanently
    // suppressed, and the backend goes straight to the browser flow.
    const res = await commands.beginPluginInstall(pluginId);
    setBusy(false);
    if (res.status === "error") {
      toast.error(res.error.message);
      return;
    }
    setBegin(res.data);
    if (res.data.oauthAvailable) {
      setStep("waitingOauth");
    } else {
      toast.error(res.data.dcrError ?? "Couldn't start the sign-in flow.");
    }
  };

  // Single resolution call: the backend runs env-var detection, RFC 8414
  // discovery, DCR, and (when possible) opens the browser. Reused by the
  // checking-step Retry and by manualClientId's re-begin.
  const runBegin = useCallback(
    async (d: PluginDetail | null) => {
      setCheckError(null);
      const res = await commands.beginPluginInstall(pluginId);
      if (res.status === "error") {
        setCheckError(res.error.message);
        return;
      }
      const r = res.data;
      setBegin(r);
      if (r.envVarPresent) {
        setStep(settingsOrDone(d));
      } else if (r.authKind === "api-key" || r.authKind === "token") {
        setStep("tokenInput");
      } else if (r.authKind !== "oauth") {
        // "none" — nothing to collect beyond [[settings]].
        setStep(settingsOrDone(d));
      } else if (r.oauthExternal) {
        setStep("manualClientId");
      } else if (r.oauthAvailable) {
        setStep("waitingOauth");
      } else if (r.needsClientId) {
        setStep("manualClientId");
      } else {
        // oauth with no endpoints (discovery failed): only Retry is possible.
        setCheckError(r.dcrError ?? "OAuth discovery failed.");
      }
    },
    [pluginId],
  );

  useEffect(() => {
    let active = true;
    void (async () => {
      // Detail first (local manifest read, fast) so the spinner can flip to
      // "Preparing sign-in…" while the network-bound begin call runs.
      const res = await commands.pluginDetail(pluginId);
      const d = res.status === "ok" ? res.data : null;
      if (!active) return;
      setDetail(d);
      await runBegin(d);
    })();
    return () => {
      active = false;
    };
  }, [pluginId, runBegin]);

  return (
    <Modal onClose={close} width={480}>
      <div className="mb-1 flex items-center gap-2.5">
        <IconChip icon={Icon} size={28} />
        <span className="text-[15px] font-semibold tracking-[-0.01em]">Install {pluginName}</span>
      </div>

      {step === "checking" && (
        <>
          {checkError ? (
            <div
              className="mt-2 flex items-start gap-2 rounded-md border border-border px-4 py-3 text-[12.5px]"
              style={{ color: "#F59E0B" }}
            >
              <CircleAlert aria-hidden size={14} strokeWidth={2} className="mt-0.5 shrink-0" />
              {checkError}
            </div>
          ) : (
            <div className="flex items-center gap-2 py-6 text-[13px] text-muted-foreground">
              <StatusDot color="#3B82F6" size={8} pulse />
              {detail?.auth?.kind === "oauth" ? "Preparing sign-in…" : "Checking configuration…"}
            </div>
          )}
          <ModalFooter>
            <Button variant="outline" onClick={close}>
              Cancel
            </Button>
            {checkError && <Button onClick={() => void runBegin(detail)}>Retry</Button>}
          </ModalFooter>
        </>
      )}

      {step === "tokenInput" && (
        <>
          <p className="mb-[18px] mt-0 text-[12.5px] text-muted-foreground">
            {pluginName} authenticates with {begin?.authKind === "api-key" ? "an API key" : "a token"}. Paste it below — Cockpit
            stores it locally and never shows it again.
          </p>
          {detail?.auth?.setting ? (
            <FormField label={begin?.authKind === "api-key" ? "API key" : "Token"}>
              <Input
                type="password"
                value={tokenValue}
                onChange={(e) => setTokenValue(e.target.value)}
                placeholder={detail?.auth?.configured ? "●●●● saved" : "Required — not set"}
              />
            </FormField>
          ) : (
            <p className="m-0 text-[12.5px] text-muted-foreground">
              This plugin reads its credential from the{" "}
              <span className="font-mono text-xs">{detail?.auth?.env ?? begin?.envVarName ?? "required"}</span> environment
              variable. Set it, restart Cockpit, and install again.
            </p>
          )}
          {detail?.auth?.helpUrl && (
            <Button
              variant="ghost"
              size="sm"
              onClick={() => void openUrl(detail.auth?.helpUrl as string)}
              className="mt-2 px-0 text-[12px] text-muted-foreground"
            >
              <ExternalLink aria-hidden size={12} strokeWidth={2} className="size-3" />
              Get a token at {detail.auth.helpUrl}
            </Button>
          )}
          <ModalFooter>
            <Button variant="outline" onClick={close}>
              Cancel
            </Button>
            <Button disabled={busy || !detail?.auth?.setting || tokenValue.trim().length === 0} onClick={() => void submitToken()}>
              {busy ? "Saving…" : "Continue"}
            </Button>
          </ModalFooter>
        </>
      )}

      {step === "manualClientId" && (
        <>
          <p className="mb-3 mt-0 text-[12.5px] text-muted-foreground">
            {begin?.oauthExternal
              ? `${pluginName} brokers its own sign-in the first time it runs. Create an OAuth client with the vendor and paste its client ID here.`
              : `${pluginName} doesn't support automatic app registration. Create an OAuth app with the vendor and paste its client ID here.`}
          </p>
          {begin?.dcrError && (
            <div
              className="mb-3 flex items-start gap-2 rounded-md border border-border px-4 py-3 text-[12.5px]"
              style={{ color: "#F59E0B" }}
            >
              <CircleAlert aria-hidden size={14} strokeWidth={2} className="mt-0.5 shrink-0" />
              {begin.dcrError}
            </div>
          )}
          <FormField label="OAuth client ID">
            <Input
              value={clientId}
              onChange={(e) => setClientId(e.target.value)}
              placeholder="Paste the client ID from the vendor's console"
            />
          </FormField>
          {detail?.auth?.helpUrl && (
            <Button
              variant="ghost"
              size="sm"
              onClick={() => void openUrl(detail.auth?.helpUrl as string)}
              className="mt-2 px-0 text-[12px] text-muted-foreground"
            >
              <ExternalLink aria-hidden size={12} strokeWidth={2} className="size-3" />
              Where do I find this?
            </Button>
          )}
          <ModalFooter>
            <Button variant="outline" onClick={close}>
              Cancel
            </Button>
            <Button disabled={busy || clientId.trim().length === 0} onClick={() => void submitClientId()}>
              {busy ? "Saving…" : "Continue"}
            </Button>
          </ModalFooter>
        </>
      )}

      {step === "waitingOauth" && (
        <>
          <p className="mb-2 mt-0 text-[13px] font-medium">Browser opened — finish signing in there.</p>
          <p className="m-0 text-[12.5px] text-muted-foreground">
            Cockpit is listening for the redirect and will finish automatically.
          </p>
          <ModalFooter>
            <Button variant="outline" onClick={close}>
              Cancel
            </Button>
          </ModalFooter>
        </>
      )}

      {step === "settings" && (
        <>
          <p className="mb-[18px] mt-0 text-[12.5px] text-muted-foreground">
            Configure {pluginName}. Required fields are marked with * — saved values stay hidden and never display.
          </p>
          <ModalFooter>
            <Button variant="outline" onClick={close}>
              Cancel
            </Button>
          </ModalFooter>
        </>
      )}

      {step === "done" && (
        <>
          <div className="flex items-center gap-2 py-4 text-[13px] font-medium">
            <Check aria-hidden size={16} strokeWidth={2.5} style={{ color: "#22C55E" }} />
            {pluginName} is installed.
          </div>
          <ModalFooter>
            <Button onClick={close}>Close</Button>
          </ModalFooter>
        </>
      )}
    </Modal>
  );
}
