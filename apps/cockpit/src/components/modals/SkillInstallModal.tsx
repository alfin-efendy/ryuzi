import { CircleAlert, Sparkles } from "lucide-react";
import { useCallback, useEffect, useState } from "react";
import { toast } from "sonner";
import { Button, FormField, Input, Modal, ModalFooter } from "@ryuzi/ui";
import { commands, type TrustPromptDto } from "@/bindings";
import { StatusDot } from "@/components/common/bits";
import { usePlugins } from "@/store-plugins";
import { LOCAL_RUNNER } from "@/lib/session-key";

const WARN = "#F59E0B";

type Step = "source" | "checking" | "trust";

function formatBytes(n: number): string {
  if (n < 1024) return `${n} B`;
  if (n < 1024 * 1024) return `${(n / 1024).toFixed(1)} KB`;
  return `${(n / (1024 * 1024)).toFixed(1)} MB`;
}

// Two-phase tiered trust gate for skill-pack installs (curated sources
// complete immediately; arbitrary sources stop here for review) — see
// `ryuzi_core::skills_install::begin_install`/`confirm_install`. Deliberately
// its own component rather than folding into `InstallWizardModal`: that
// wizard drives an OAuth state machine (env var/token/browser flow/settings)
// that has nothing to do with skill packs, which only ever need a source
// string and, sometimes, a trust acknowledgment.
export function SkillInstallModal({
  initialSource,
  onClose,
}: {
  /** Pre-known source (Browse tab quick-install). Omit to show the manual
   *  "owner/repo" entry step (the "Add skill source" flow). */
  initialSource?: string;
  onClose: () => void;
}) {
  const loadPlugins = usePlugins((s) => s.load);
  const [step, setStep] = useState<Step>(initialSource ? "checking" : "source");
  const [source, setSource] = useState(initialSource ?? "");
  const [trust, setTrust] = useState<TrustPromptDto | null>(null);
  const [busy, setBusy] = useState(false);

  const begin = useCallback(
    async (target: string) => {
      setBusy(true);
      const res = await commands.beginSkillInstall(LOCAL_RUNNER, target);
      setBusy(false);
      if (res.status === "error") {
        toast.error(`Skill install failed: ${res.error.message}`);
        setStep("source");
        return;
      }
      if (res.data.completed) {
        toast.success(`${res.data.plugin?.name ?? target} installed`);
        await loadPlugins();
        onClose();
        return;
      }
      setTrust(res.data.trust);
      setStep("trust");
    },
    [loadPlugins, onClose],
  );

  // Browse-tab quick installs already know the source — begin immediately
  // instead of asking the user to retype it. Deliberately mount-only: re-firing
  // on every `begin`/`initialSource` identity change would re-trigger the
  // network call, not just react to a prop update.
  // biome-ignore lint/correctness/useExhaustiveDependencies: mount-only, see comment above
  useEffect(() => {
    if (initialSource) void begin(initialSource);
  }, []);

  const submitSource = async () => {
    const target = source.trim();
    if (target === "" || busy) return;
    setStep("checking");
    await begin(target);
  };

  const confirm = async () => {
    if (!trust || busy) return;
    setBusy(true);
    const res = await commands.confirmSkillInstall(LOCAL_RUNNER, trust.token);
    setBusy(false);
    if (res.status === "error") {
      toast.error(`Skill install failed: ${res.error.message}`);
      return;
    }
    toast.success(`${res.data.name} installed`);
    await loadPlugins();
    onClose();
  };

  return (
    <Modal onClose={onClose} width={460}>
      <div className="mb-1 flex items-center gap-2.5">
        <Sparkles aria-hidden size={18} strokeWidth={2} className="text-muted-foreground" />
        <span className="text-[15px] font-semibold tracking-[-0.01em]">Install a skill source</span>
      </div>

      {step === "source" && (
        <>
          <FormField label="Skill source" hint="A GitHub repo (owner/repo) containing agent skills.">
            <Input value={source} onChange={(e) => setSource(e.target.value)} placeholder="owner/repo" aria-label="Skill source" />
          </FormField>
          <ModalFooter>
            <Button variant="outline" onClick={onClose}>
              Cancel
            </Button>
            <Button onClick={() => void submitSource()} disabled={busy || source.trim() === ""}>
              Install
            </Button>
          </ModalFooter>
        </>
      )}

      {step === "checking" && (
        <>
          <div className="flex items-center gap-2 py-6 text-[13px] text-muted-foreground">
            <StatusDot color="#3B82F6" size={8} pulse />
            Checking source…
          </div>
          <ModalFooter>
            <Button variant="outline" onClick={onClose}>
              Cancel
            </Button>
          </ModalFooter>
        </>
      )}

      {step === "trust" && trust && (
        <>
          <p className="mb-3 mt-0 text-[12.5px] text-muted-foreground">
            This source isn't a curated pack — review what it installs before Cockpit trusts it.
          </p>
          <div className="flex flex-col gap-2 rounded-md border border-border px-4 py-3 text-[12.5px]">
            <div>
              <span className="font-medium">Source: </span>
              <span className="font-mono text-xs">{trust.sourceSpec}</span>
            </div>
            {trust.ownerRepo !== trust.sourceSpec && (
              <div>
                <span className="font-medium">Repository: </span>
                <span className="font-mono text-xs">{trust.ownerRepo}</span>
              </div>
            )}
            {trust.resolvedCommit && (
              <div>
                <span className="font-medium">Commit: </span>
                <span className="font-mono text-xs">{trust.resolvedCommit.slice(0, 12)}</span>
              </div>
            )}
            <div>
              <span className="font-medium">Size: </span>
              {formatBytes(trust.totalBytes)}
            </div>
          </div>

          {trust.skills.length > 0 && (
            <div className="mt-3">
              <div className="mb-1 text-[12.5px] font-medium">Skills ({trust.skills.length})</div>
              <ul className="m-0 list-none rounded-md border border-border p-0 text-[12px] text-muted-foreground">
                {trust.skills.map((s) => (
                  <li key={s} className="border-b border-border px-3 py-1.5 font-mono last:border-b-0">
                    {s}
                  </li>
                ))}
              </ul>
            </div>
          )}

          {trust.hookScripts.length > 0 && (
            <div
              className="mt-3 flex flex-col gap-1.5 rounded-md border px-3 py-2.5 text-[12px]"
              style={{ borderColor: WARN, color: WARN }}
            >
              <div className="flex items-center gap-2 font-medium">
                <CircleAlert aria-hidden size={14} strokeWidth={2} className="shrink-0" />
                Hook scripts ({trust.hookScripts.length}) — these run automatically when triggered
              </div>
              <ul className="m-0 list-none p-0 pl-[22px] font-mono">
                {trust.hookScripts.map((h) => (
                  <li key={h}>{h}</li>
                ))}
              </ul>
            </div>
          )}

          <ModalFooter>
            <Button variant="outline" onClick={onClose}>
              Cancel
            </Button>
            <Button onClick={() => void confirm()} disabled={busy}>
              {busy ? "Installing…" : "Trust & Install"}
            </Button>
          </ModalFooter>
        </>
      )}
    </Modal>
  );
}
