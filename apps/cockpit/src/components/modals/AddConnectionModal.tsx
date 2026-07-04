import { useEffect, useRef, useState } from "react";
import { ArrowLeft, Loader2 } from "lucide-react";
import { useConnections } from "@/store-connections";
import type { CatalogEntry, ManualStartInfo } from "@/bindings";
import { Chip, Pill } from "@/components/common/bits";
import { Button, FormField, Input, Modal, ModalFooter } from "@ryuzi/ui";

// "kiro" is a Free-category catalog entry that isn't wired up yet (it needs a
// base URL the free-add flow doesn't collect) — keep it greyed "Coming soon".
const NOT_YET_WIRED = new Set(["kiro"]);

type OauthStep = "form" | "waiting-browser" | "manual";

// Multi-step flow: pick a provider from the catalog, then walk its connect
// path — API key form, OAuth browser/paste, or a direct Free add.
export function AddConnectionModal({ open, onClose }: { open: boolean; onClose: () => void }) {
  const { catalog, add, connectOauth, beginOauthManual, completeOauthManual, addFree } = useConnections();
  const [picked, setPicked] = useState<CatalogEntry | null>(null);
  const [label, setLabel] = useState("");
  const [apiKey, setApiKey] = useState("");
  const [baseUrl, setBaseUrl] = useState("");
  const [saving, setSaving] = useState(false);
  const [oauthStep, setOauthStep] = useState<OauthStep>("form");
  const [manualInfo, setManualInfo] = useState<ManualStartInfo | null>(null);
  const [pasted, setPasted] = useState("");

  // Tracks the "currently picked" provider even inside async closures that
  // captured an earlier `picked` value — lets in-flight requests detect that
  // the user backed out and switched providers while they were awaiting.
  const pickedRef = useRef<CatalogEntry | null>(picked);
  useEffect(() => {
    pickedRef.current = picked;
  }, [picked]);

  if (!open) return null;

  const reset = () => {
    setPicked(null);
    setLabel("");
    setApiKey("");
    setBaseUrl("");
    setSaving(false);
    setOauthStep("form");
    setManualInfo(null);
    setPasted("");
  };
  const close = () => {
    reset();
    onClose();
  };

  const baseUrlMissing = !!picked?.requiresBaseUrl && baseUrl.trim().length === 0;

  const submit = async () => {
    if (!picked || saving || baseUrlMissing) return;
    setSaving(true);
    const ok = await add(picked.id, label.trim() || picked.name, apiKey, baseUrl.trim() || null);
    setSaving(false);
    if (ok) close();
  };

  const submitFree = async () => {
    if (!picked || saving) return;
    setSaving(true);
    const ok = await addFree(picked.id, label.trim() || picked.name);
    setSaving(false);
    if (ok) close();
  };

  const connectBrowser = async () => {
    if (!picked || saving) return;
    const target = picked;
    setSaving(true);
    setOauthStep("waiting-browser");
    const ok = await connectOauth(target.id, label.trim() || target.name);
    // The user may have backed out and picked a different provider while
    // this was in flight — if so, this result is stale and must not touch
    // state that now belongs to the new pick (e.g. force-closing the modal).
    if (pickedRef.current?.id !== target.id) return;
    setSaving(false);
    if (ok) close();
    else setOauthStep("form");
  };

  const startManual = async () => {
    if (!picked || saving) return;
    const target = picked;
    setSaving(true);
    const info = await beginOauthManual(target.id);
    if (pickedRef.current?.id !== target.id) return;
    setSaving(false);
    if (info) {
      setManualInfo(info);
      setOauthStep("manual");
    }
  };

  const submitManual = async () => {
    if (!picked || !manualInfo || saving || pasted.trim().length === 0) return;
    const target = picked;
    const targetInfo = manualInfo;
    setSaving(true);
    const ok = await completeOauthManual(
      target.id,
      label.trim() || target.name,
      targetInfo.verifier,
      targetInfo.state,
      pasted.trim(),
      targetInfo.redirectUri,
    );
    if (pickedRef.current?.id !== target.id) return;
    setSaving(false);
    if (ok) close();
  };

  return (
    <Modal onClose={close} width={480}>
      {picked === null ? (
        <>
          <div className="text-[15px] font-semibold tracking-[-0.01em]">Add connection</div>
          <p className="mb-4 mt-1 text-[12.5px] text-muted-foreground">Pick a provider to connect.</p>
          <div className="grid grid-cols-2 gap-2">
            {catalog.map((ci) => {
              // `saving` means an oauth/api-key/free request is in flight for the
              // currently picked provider — block switching providers mid-flight
              // so a late-resolving promise can't apply to the wrong pick.
              const disabled = NOT_YET_WIRED.has(ci.id) || saving;
              return (
                <Button
                  key={ci.id}
                  variant="outline"
                  disabled={disabled}
                  onClick={() => setPicked(ci)}
                  className="h-auto w-full justify-start gap-[11px] px-3 py-[11px] text-left"
                >
                  <Chip initial={ci.initial} color={ci.color} size={32} />
                  <span className="min-w-0 flex-1">
                    <span className="flex items-center gap-1.5 font-semibold">
                      {ci.name}
                      {disabled && <Pill>Coming soon</Pill>}
                    </span>
                    <span className="block overflow-hidden text-ellipsis whitespace-nowrap text-[11px] font-normal text-muted-foreground">
                      {ci.category === "oauth"
                        ? "Sign in with browser"
                        : ci.category === "free"
                          ? "No credentials needed"
                          : ci.format === "anthropic"
                            ? "Anthropic-compatible"
                            : "OpenAI-compatible"}
                    </span>
                  </span>
                </Button>
              );
            })}
          </div>
          <ModalFooter className="mt-[18px]">
            <Button variant="outline" onClick={close}>
              Cancel
            </Button>
          </ModalFooter>
        </>
      ) : (
        <>
          <div className="flex items-center gap-3">
            <Chip initial={picked.initial} color={picked.color} size={36} />
            <div className="min-w-0 flex-1">
              <div className="text-[15px] font-semibold tracking-[-0.01em]">Add connection</div>
              <div className="text-xs text-muted-foreground">{picked.name}</div>
            </div>
          </div>

          {picked.category === "api_key" && (
            <>
              <div className="mt-3.5 flex flex-col gap-3">
                <FormField label="Label">
                  <Input value={label} onChange={(e) => setLabel(e.target.value)} placeholder={picked.name} />
                </FormField>
                <FormField label="API key">
                  <Input type="password" value={apiKey} onChange={(e) => setApiKey(e.target.value)} placeholder="sk-…" />
                </FormField>
                <FormField
                  label={
                    <>
                      {picked.requiresBaseUrl ? "Base URL" : "Base URL override"}
                      {!picked.requiresBaseUrl && <span className="font-normal text-muted-foreground"> — optional</span>}
                    </>
                  }
                >
                  <Input value={baseUrl} onChange={(e) => setBaseUrl(e.target.value)} placeholder="https://host/v1" />
                </FormField>
              </div>

              <Button size="lg" onClick={() => void submit()} disabled={saving || baseUrlMissing} className="mt-3.5 w-full">
                {saving ? "Adding…" : `Add ${picked.name}`}
              </Button>
            </>
          )}

          {picked.category === "free" && (
            <>
              <div className="mt-3.5 flex flex-col gap-3">
                <FormField label="Label">
                  <Input value={label} onChange={(e) => setLabel(e.target.value)} placeholder={picked.name} />
                </FormField>
              </div>
              <p className="mt-2 text-[11.5px] text-muted-foreground">No credentials required — this connects immediately.</p>

              <Button size="lg" onClick={() => void submitFree()} disabled={saving} className="mt-2 w-full">
                {saving ? "Adding…" : `Add ${picked.name}`}
              </Button>
            </>
          )}

          {picked.category === "oauth" && (
            <>
              {oauthStep === "form" && (
                <>
                  <div className="mt-3.5 flex flex-col gap-3">
                    <FormField label="Label">
                      <Input value={label} onChange={(e) => setLabel(e.target.value)} placeholder={picked.name} />
                    </FormField>
                  </div>
                  <Button size="lg" onClick={() => void connectBrowser()} disabled={saving} className="mt-3.5 w-full">
                    Connect with browser
                  </Button>
                  <Button size="lg" variant="outline" onClick={() => void startManual()} disabled={saving} className="mt-2 w-full">
                    {saving ? "Opening…" : "Paste code instead"}
                  </Button>
                </>
              )}

              {oauthStep === "waiting-browser" && (
                <div className="mt-3.5 flex items-center gap-2 rounded-md border border-border px-3 py-2.5 text-[12.5px] text-muted-foreground">
                  <Loader2 aria-hidden size={13} strokeWidth={2} className="shrink-0 animate-spin" />
                  Waiting for your browser… complete the login, then return here.
                </div>
              )}

              {oauthStep === "manual" && (
                <div className="mt-3.5 flex flex-col gap-3">
                  <p className="text-[12.5px] text-muted-foreground">
                    We opened your browser to sign in to {picked.name}. Paste the code or redirect URL it gave you below.
                  </p>
                  <FormField label="Code or redirect URL">
                    <textarea
                      className="min-h-[72px] w-full resize-y rounded-md border border-input bg-background px-3 py-2 font-sans text-[12.5px] text-foreground"
                      value={pasted}
                      onChange={(e) => setPasted(e.target.value)}
                      placeholder="Paste here"
                    />
                  </FormField>
                  <Button size="lg" onClick={() => void submitManual()} disabled={saving || pasted.trim().length === 0} className="w-full">
                    {saving ? "Connecting…" : "Submit"}
                  </Button>
                </div>
              )}
            </>
          )}

          <ModalFooter className="mt-4">
            <Button variant="ghost" onClick={reset} disabled={saving} className="text-muted-foreground">
              <ArrowLeft aria-hidden size={12} strokeWidth={2} className="size-3" />
              Back
            </Button>
            <div className="flex-1" />
            <Button variant="outline" onClick={close}>
              Cancel
            </Button>
          </ModalFooter>
        </>
      )}
    </Modal>
  );
}
