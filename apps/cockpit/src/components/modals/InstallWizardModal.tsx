import { Check, CircleAlert } from "lucide-react";
import { useCallback, useEffect, useState } from "react";
import { Button, Modal, ModalFooter } from "@ryuzi/ui";
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

  const Icon = iconFor(pluginIcon ?? null);

  const close = () => {
    onClose();
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
          <ModalFooter>
            <Button variant="outline" onClick={close}>
              Cancel
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
          <ModalFooter>
            <Button variant="outline" onClick={close}>
              Cancel
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
