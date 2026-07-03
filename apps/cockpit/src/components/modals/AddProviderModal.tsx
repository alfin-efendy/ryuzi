import { useState } from "react";
import { ArrowLeft } from "lucide-react";
import { PROVIDER_CATALOG } from "@/constants";
import { useProviders } from "@/store-providers";
import { Chip } from "@/components/common/bits";
import { Modal } from "./Modal";

type CatalogEntry = (typeof PROVIDER_CATALOG)[number];

const cancelBtn =
  "cursor-pointer rounded-md border border-border bg-transparent font-sans text-[12.5px] font-medium text-foreground hover:bg-accent";
const field = "h-9 rounded-md border border-input bg-background px-3 font-sans text-[12.5px] text-foreground";

// Two-step flow: pick a provider from the catalog, then record an account.
// Credentials stay with the agent CLIs (e.g. `claude login`) — Cockpit stores
// provider/account config and tracks usage locally against the limits you set.
export function AddProviderModal({
  open,
  onClose,
  forProviderId,
}: {
  open: boolean;
  onClose: () => void;
  /** When set, skips the catalog step and adds an account to this provider. */
  forProviderId?: string;
}) {
  const { providers, add, addAccount } = useProviders();
  const [picked, setPicked] = useState<CatalogEntry | null>(null);
  const [label, setLabel] = useState("");
  const [email, setEmail] = useState("");
  const [plan, setPlan] = useState("");
  const [sessionLimit, setSessionLimit] = useState("");
  const [weeklyLimit, setWeeklyLimit] = useState("");
  const [saving, setSaving] = useState(false);
  if (!open) return null;

  const target = forProviderId ? (PROVIDER_CATALOG.find((c) => c.id === forProviderId) ?? null) : picked;

  const reset = () => {
    setPicked(null);
    setLabel("");
    setEmail("");
    setPlan("");
    setSessionLimit("");
    setWeeklyLimit("");
  };
  const close = () => {
    reset();
    onClose();
  };

  const parseLimit = (v: string): number | null => {
    const n = Number(v.replace(/[,._\s]/g, ""));
    return Number.isFinite(n) && n > 0 ? Math.round(n * 1_000_000) : null;
  };

  const submit = async () => {
    if (!target || saving) return;
    setSaving(true);
    let ok = true;
    if (!providers.some((p) => p.id === target.id)) {
      ok = await add(target.id, target.name, target.kind, target.color);
    }
    if (ok) {
      ok = await addAccount(
        target.id,
        label.trim() || "Account 1",
        email.trim(),
        plan.trim(),
        parseLimit(sessionLimit),
        parseLimit(weeklyLimit),
      );
    }
    setSaving(false);
    if (ok) close();
  };

  return (
    <Modal onClose={close} width={480}>
      {target === null ? (
        <>
          <div className="text-[15px] font-semibold tracking-[-0.01em]">Connect provider</div>
          <p className="mb-4 mt-1 text-[12.5px] text-muted-foreground">
            Pick a provider, then record the account it should track.
          </p>
          <div className="grid grid-cols-2 gap-2">
            {PROVIDER_CATALOG.map((ci) => (
              <button
                key={ci.id}
                type="button"
                onClick={() => setPicked(ci)}
                className="flex cursor-pointer items-center gap-[11px] rounded-lg border border-border bg-transparent px-3 py-[11px] text-left font-sans text-popover-foreground hover:bg-accent"
              >
                <Chip initial={ci.initial} color={ci.color} size={32} />
                <span className="min-w-0 flex-1">
                  <span className="flex items-center gap-1.5 text-[13px] font-semibold">
                    {ci.name}
                    {providers.some((p) => p.id === ci.id) && (
                      <span className="rounded-full bg-secondary px-1.5 py-px text-[9.5px] font-semibold uppercase tracking-[0.03em] text-secondary-foreground">
                        Added
                      </span>
                    )}
                  </span>
                  <span className="block overflow-hidden text-ellipsis whitespace-nowrap text-[11px] text-muted-foreground">{ci.kind}</span>
                </span>
              </button>
            ))}
          </div>
          <div className="mt-[18px] flex justify-end">
            <button type="button" onClick={close} className={`${cancelBtn} h-8 px-3.5`}>
              Cancel
            </button>
          </div>
        </>
      ) : (
        <>
          <div className="flex items-center gap-3">
            <Chip initial={target.initial} color={target.color} size={36} />
            <div className="min-w-0 flex-1">
              <div className="text-[15px] font-semibold tracking-[-0.01em]">
                {forProviderId ? "Add account" : "Connect provider"}
              </div>
              <div className="text-xs text-muted-foreground">{target.name}</div>
            </div>
          </div>

          <p className="mb-3.5 mt-3 text-[12.5px] leading-[1.55] text-muted-foreground">
            Credentials stay with the agent CLI (e.g. <span className="font-mono text-xs">claude login</span>). Cockpit records the
            account and tracks estimated usage locally against the limits you set.
          </p>

          <div className="flex flex-col gap-3">
            <div className="flex gap-3">
              <label className="flex flex-1 flex-col gap-1.5">
                <span className="text-xs font-semibold">Label</span>
                <input className={field} value={label} onChange={(e) => setLabel(e.target.value)} placeholder="Account 1" />
              </label>
              <label className="flex flex-1 flex-col gap-1.5">
                <span className="text-xs font-semibold">Plan</span>
                <input className={field} value={plan} onChange={(e) => setPlan(e.target.value)} placeholder="Max 20×" />
              </label>
            </div>
            <label className="flex flex-col gap-1.5">
              <span className="text-xs font-semibold">Email / identifier</span>
              <input className={field} value={email} onChange={(e) => setEmail(e.target.value)} placeholder="you@example.com" />
            </label>
            <div className="flex gap-3">
              <label className="flex flex-1 flex-col gap-1.5">
                <span className="text-xs font-semibold">Session limit (M tokens / 5h)</span>
                <input
                  className={field}
                  value={sessionLimit}
                  onChange={(e) => setSessionLimit(e.target.value)}
                  placeholder="e.g. 5"
                />
              </label>
              <label className="flex flex-1 flex-col gap-1.5">
                <span className="text-xs font-semibold">Weekly limit (M tokens)</span>
                <input
                  className={field}
                  value={weeklyLimit}
                  onChange={(e) => setWeeklyLimit(e.target.value)}
                  placeholder="e.g. 40"
                />
              </label>
            </div>
            <span className="text-[11.5px] text-muted-foreground">
              Limits are optional — set them to get local quota bars against real usage.
            </span>
          </div>

          <button
            type="button"
            onClick={() => void submit()}
            disabled={saving}
            className="mt-3.5 flex h-9 w-full cursor-pointer items-center justify-center gap-2 rounded-md border-none bg-primary font-sans text-[13px] font-medium text-primary-foreground hover:opacity-85 disabled:opacity-50"
          >
            {saving ? "Saving…" : forProviderId ? "Add account" : `Add ${target.name}`}
          </button>

          <div className="mt-4 flex items-center gap-2">
            {!forProviderId && (
              <button
                type="button"
                onClick={reset}
                className="flex h-[30px] cursor-pointer items-center gap-1.5 rounded-md border-none bg-transparent px-2.5 font-sans text-[12.5px] font-medium text-muted-foreground hover:bg-accent hover:text-accent-foreground"
              >
                <ArrowLeft aria-hidden size={12} strokeWidth={2} />
                Back
              </button>
            )}
            <div className="flex-1" />
            <button type="button" onClick={close} className={`${cancelBtn} h-[30px] px-3`}>
              Cancel
            </button>
          </div>
        </>
      )}
    </Modal>
  );
}
