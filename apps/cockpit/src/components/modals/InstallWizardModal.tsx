import { openUrl } from "@tauri-apps/plugin-opener";
import { Check, CircleAlert, ExternalLink } from "lucide-react";
import { useCallback, useEffect, useRef, useState } from "react";
import { toast } from "sonner";
import { Button, FormField, Input, Modal, ModalFooter } from "@ryuzi/ui";
import { commands, events, type PluginDetail, type PluginInstallBeginResult } from "@/bindings";
import { IconChip, StatusDot } from "@/components/common/bits";
import { pluginIcon as iconFor } from "@/lib/plugin-icons";
import { usePlugins } from "@/store-plugins";

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
  const loadPlugins = usePlugins((s) => s.load);
  const [step, setStep] = useState<WizardStep>("checking");
  const [detail, setDetail] = useState<PluginDetail | null>(null);
  const [begin, setBegin] = useState<PluginInstallBeginResult | null>(null);
  const [checkError, setCheckError] = useState<string | null>(null);
  const [tokenValue, setTokenValue] = useState("");
  const [clientId, setClientId] = useState("");
  const [oauthError, setOauthError] = useState<string | null>(null);
  const [pasteOpen, setPasteOpen] = useState(false);
  const [code, setCode] = useState("");
  const [fieldValues, setFieldValues] = useState<Record<string, string>>({});
  const [busy, setBusy] = useState(false);

  const Icon = iconFor(pluginIcon ?? null);

  // Close/Cancel from any step. For oauth installs also tear down the
  // pending flow (loopback listener + flow-map entry). Uses the manifest's
  // auth kind while begin is still in flight — the backend flow may already
  // have started. cancel_plugin_install is a safe no-op when nothing is
  // pending (including after a completed flow).
  const close = () => {
    const oauthFlow = begin ? begin.authKind === "oauth" : detail?.auth?.kind === "oauth";
    if (oauthFlow) {
      void commands.cancelPluginInstall(pluginId, begin?.oauthBegin?.stateToken ?? null);
    }
    onClose();
  };

  // Latest-ref mirror of pluginId/begin/detail, kept in sync on every render
  // so the unmount-only effect below can read fresh values in its cleanup
  // without needing them in its dependency array (which would re-subscribe
  // the effect — and re-firing on every begin/detail change is not what we
  // want for an unmount safety net).
  const wizardStateRef = useRef({ pluginId, begin, detail });
  wizardStateRef.current = { pluginId, begin, detail };

  // Safety net for unmounts that skip close() entirely — e.g. sidebar
  // navigation away from the modal's host route (Tab-bypass) — and shrinks
  // the window where a close-during-begin race could leave a flow dangling.
  // Mirrors close()'s oauth-cancellation logic exactly. cancel_plugin_install
  // is an idempotent no-op when nothing is pending, so firing this after a
  // normal close() already cancelled the same flow is harmless.
  useEffect(() => {
    return () => {
      const { pluginId: pid, begin: b, detail: d } = wizardStateRef.current;
      const oauthFlow = b ? b.authKind === "oauth" : d?.auth?.kind === "oauth";
      if (oauthFlow) {
        void commands.cancelPluginInstall(pid, b?.oauthBegin?.stateToken ?? null);
      }
    };
  }, []);

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

  const retryOauth = async () => {
    if (busy) return;
    setBusy(true);
    setOauthError(null);
    const res = await commands.beginPluginInstall(pluginId);
    setBusy(false);
    if (res.status === "error") {
      setOauthError(res.error.message);
      return;
    }
    setBegin(res.data);
    if (!res.data.oauthAvailable) setOauthError(res.data.dcrError ?? "Couldn't restart the sign-in flow.");
  };

  const submitCode = async () => {
    const stateToken = begin?.oauthBegin?.stateToken;
    if (!stateToken || code.trim().length === 0 || busy) return;
    setBusy(true);
    const res = await commands.completePluginOauth(pluginId, code.trim(), stateToken);
    setBusy(false);
    if (res.status === "error") {
      setOauthError(res.error.message);
      return;
    }
    setOauthError(null);
    // The loopback callback server is still listening for this flow's
    // redirect — a manual paste bypasses it, so shut it down explicitly or
    // it leaks until the flow's own timeout (Phase 1 whole-branch review).
    await commands.cancelPluginInstall(pluginId, stateToken);
    setStep(settingsOrDone(detail));
  };

  const settingsFields = detail?.settings ?? [];
  // valueSet counts (a saved value satisfies the requirement without ever
  // being echoed); a non-empty typed value counts because Continue saves it.
  const requiredSatisfied = settingsFields.every((f) => !f.required || f.valueSet || (fieldValues[f.key] ?? "").trim().length > 0);

  const submitSettings = async () => {
    if (busy || !requiredSatisfied) return;
    setBusy(true);
    for (const f of settingsFields) {
      const value = (fieldValues[f.key] ?? "").trim();
      if (value.length === 0) continue;
      const res = await commands.setPluginSetting(f.key, value);
      if (res.status === "error") {
        setBusy(false);
        toast.error(res.error.message);
        return;
      }
    }
    setBusy(false);
    setStep("done");
  };

  // Single resolution call: the backend runs env-var detection, RFC 8414
  // discovery, DCR, and (when possible) opens the browser. Used at mount and
  // by the checking-step Retry. manualClientId's submitClientId deliberately
  // does NOT call this — it hand-rolls a reduced re-begin so failures toast
  // instead of setting checkError, reusing the mount-time detail.
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

  // Phase 1's loopback callback server emits pluginOauthCompletedMsg once it
  // captured + exchanged the code (or failed). Same listen/cleanup pattern
  // as PluginDetailView's pluginOauthAuthorizeUrlMsg subscription.
  useEffect(() => {
    if (step !== "waitingOauth") return;
    let active = true;
    let unlisten: (() => void) | null = null;

    void events.pluginOauthCompletedMsg
      .listen((event) => {
        if (!active || event.payload.pluginId !== pluginId) return;
        if (event.payload.ok) {
          setOauthError(null);
          setStep(settingsOrDone(detail));
        } else {
          setOauthError(event.payload.error ?? "Sign-in didn't finish.");
        }
      })
      .then((stop) => {
        if (active) unlisten = stop;
        else stop();
      });

    return () => {
      active = false;
      unlisten?.();
    };
  }, [step, pluginId, detail]);

  // Entering "done" is the commit point: enable the plugin (experimental
  // plugins keep the existing gate and are never auto-enabled), then refresh
  // the plugins store so the Browse card flips to Open + Switch.
  useEffect(() => {
    if (step !== "done") return;
    let active = true;
    void (async () => {
      if (detail && !detail.info.experimental) {
        const res = await commands.setPluginEnabled(pluginId, true);
        if (!active) return;
        if (res.status === "error") toast.error(res.error.message);
      }
      if (!active) return;
      await loadPlugins();
    })();
    return () => {
      active = false;
    };
  }, [step, pluginId, detail, loadPlugins]);

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
            {pluginName} authenticates with {begin?.authKind === "api-key" ? "an API key" : "a token"}. Paste it below — Cockpit stores it
            locally and never shows it again.
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
              <span className="font-mono text-xs">{detail?.auth?.env ?? begin?.envVarName ?? "required"}</span> environment variable. Set
              it, restart Cockpit, and install again.
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
          <p className="m-0 text-[12.5px] text-muted-foreground">Cockpit is listening for the redirect and will finish automatically.</p>
          {oauthError && (
            <div className="mt-3 flex flex-col gap-2 rounded-md border border-border px-4 py-3 text-[12.5px]" style={{ color: "#F59E0B" }}>
              <span className="flex items-start gap-2">
                <CircleAlert aria-hidden size={14} strokeWidth={2} className="mt-0.5 shrink-0" />
                {oauthError}
              </span>
              <span className="flex gap-2">
                <Button variant="outline" size="sm" onClick={() => void retryOauth()} disabled={busy}>
                  Retry
                </Button>
                {!pasteOpen && begin?.callbackMode !== "manual" && (
                  <Button variant="outline" size="sm" onClick={() => setPasteOpen(true)}>
                    Paste code instead
                  </Button>
                )}
              </span>
            </div>
          )}
          {(pasteOpen || begin?.callbackMode === "manual") && (
            <div className="mt-3">
              {begin?.callbackMode === "manual" && (
                <p className="mb-2 mt-0 text-xs text-muted-foreground">
                  Another sign-in is holding the callback port, so Cockpit can't catch the redirect. After signing in, copy the{" "}
                  <span className="font-mono">code</span> value from the browser's address bar and paste it here.
                </p>
              )}
              <FormField label="Authorization code">
                <Input value={code} onChange={(e) => setCode(e.target.value)} placeholder="Paste the code value from the callback URL" />
              </FormField>
              <div className="mt-2 flex justify-end">
                <Button
                  size="sm"
                  disabled={busy || code.trim().length === 0 || !begin?.oauthBegin?.stateToken}
                  onClick={() => void submitCode()}
                >
                  {busy ? "Connecting…" : "Finish sign-in"}
                </Button>
              </div>
            </div>
          )}
          {!pasteOpen && begin?.callbackMode !== "manual" && !oauthError && (
            <Button variant="ghost" size="sm" onClick={() => setPasteOpen(true)} className="mt-3 px-0 text-[12px] text-muted-foreground">
              Having trouble? Paste the code manually
            </Button>
          )}
          <ModalFooter>
            <Button variant="outline" onClick={close}>
              Cancel
            </Button>
            <Button
              variant="outline"
              onClick={() => void openUrl(begin?.oauthBegin?.authorizeUrl ?? "")}
              disabled={!begin?.oauthBegin?.authorizeUrl}
            >
              Reopen browser
            </Button>
          </ModalFooter>
        </>
      )}

      {step === "settings" && (
        <>
          <p className="mb-[18px] mt-0 text-[12.5px] text-muted-foreground">
            Configure {pluginName}. Required fields are marked with * — saved values stay hidden and never display.
          </p>
          <div className="flex flex-col gap-3">
            {settingsFields.map((f) => (
              <FormField key={f.key} label={f.required ? `${f.label} *` : f.label} hint={f.help || undefined}>
                <Input
                  type={f.secret ? "password" : "text"}
                  value={fieldValues[f.key] ?? ""}
                  onChange={(e) => setFieldValues((m) => ({ ...m, [f.key]: e.target.value }))}
                  placeholder={f.valueSet ? "●●●● saved" : f.required ? "Required — not set" : "Optional — not set"}
                />
              </FormField>
            ))}
          </div>
          <ModalFooter>
            <Button variant="outline" onClick={close}>
              Cancel
            </Button>
            <Button disabled={busy || !requiredSatisfied} onClick={() => void submitSettings()}>
              {busy ? "Saving…" : "Continue"}
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
          <p className="m-0 text-[12.5px] text-muted-foreground">
            {detail == null
              ? "You can enable it from the card."
              : detail.info.experimental
                ? "It's experimental, so it stays off — enable it from the card when ready."
                : "It's enabled and ready for your agents."}
          </p>
          <ModalFooter>
            <Button onClick={close}>Close</Button>
          </ModalFooter>
        </>
      )}
    </Modal>
  );
}
