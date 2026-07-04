import { useState } from "react";
import { ArrowLeft } from "lucide-react";
import { useConnections } from "@/store-connections";
import type { CatalogEntry } from "@/bindings";
import { Chip, Pill } from "@/components/common/bits";
import { Modal } from "./Modal";

const cancelBtn =
  "cursor-pointer rounded-md border border-border bg-transparent font-sans text-[12.5px] font-medium text-foreground hover:bg-accent";
const field = "h-9 rounded-md border border-input bg-background px-3 font-sans text-[12.5px] text-foreground";

// Two-step flow: pick a provider from the catalog, then supply its API key.
// Only "api_key" category entries are wired up in this milestone — oauth/free
// entries are shown greyed out with a "Coming soon" badge.
export function AddConnectionModal({ open, onClose }: { open: boolean; onClose: () => void }) {
  const { catalog, add } = useConnections();
  const [picked, setPicked] = useState<CatalogEntry | null>(null);
  const [label, setLabel] = useState("");
  const [apiKey, setApiKey] = useState("");
  const [baseUrl, setBaseUrl] = useState("");
  const [saving, setSaving] = useState(false);
  if (!open) return null;

  const reset = () => {
    setPicked(null);
    setLabel("");
    setApiKey("");
    setBaseUrl("");
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

  return (
    <Modal onClose={close} width={480}>
      {picked === null ? (
        <>
          <div className="text-[15px] font-semibold tracking-[-0.01em]">Add connection</div>
          <p className="mb-4 mt-1 text-[12.5px] text-muted-foreground">Pick a provider to connect with an API key.</p>
          <div className="grid grid-cols-2 gap-2">
            {catalog.map((ci) => {
              const disabled = ci.category !== "api_key";
              return (
                <button
                  key={ci.id}
                  type="button"
                  disabled={disabled}
                  onClick={() => setPicked(ci)}
                  className={`flex cursor-pointer items-center gap-[11px] rounded-lg border border-border bg-transparent px-3 py-[11px] text-left font-sans text-popover-foreground hover:bg-accent ${
                    disabled ? "pointer-events-none opacity-50" : ""
                  }`}
                >
                  <Chip initial={ci.initial} color={ci.color} size={32} />
                  <span className="min-w-0 flex-1">
                    <span className="flex items-center gap-1.5 text-[13px] font-semibold">
                      {ci.name}
                      {disabled && <Pill>Coming soon</Pill>}
                    </span>
                    <span className="block overflow-hidden text-ellipsis whitespace-nowrap text-[11px] text-muted-foreground">
                      {ci.format === "anthropic" ? "Anthropic-compatible" : "OpenAI-compatible"}
                    </span>
                  </span>
                </button>
              );
            })}
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
            <Chip initial={picked.initial} color={picked.color} size={36} />
            <div className="min-w-0 flex-1">
              <div className="text-[15px] font-semibold tracking-[-0.01em]">Add connection</div>
              <div className="text-xs text-muted-foreground">{picked.name}</div>
            </div>
          </div>

          <div className="mt-3.5 flex flex-col gap-3">
            <label className="flex flex-col gap-1.5">
              <span className="text-xs font-semibold">Label</span>
              <input className={field} value={label} onChange={(e) => setLabel(e.target.value)} placeholder={picked.name} />
            </label>
            <label className="flex flex-col gap-1.5">
              <span className="text-xs font-semibold">API key</span>
              <input type="password" className={field} value={apiKey} onChange={(e) => setApiKey(e.target.value)} placeholder="sk-…" />
            </label>
            <label className="flex flex-col gap-1.5">
              <span className="text-xs font-semibold">
                {picked.requiresBaseUrl ? "Base URL" : "Base URL override"}
                {!picked.requiresBaseUrl && <span className="font-normal text-muted-foreground"> — optional</span>}
              </span>
              <input className={field} value={baseUrl} onChange={(e) => setBaseUrl(e.target.value)} placeholder="https://host/v1" />
            </label>
          </div>

          <button
            type="button"
            onClick={() => void submit()}
            disabled={saving || baseUrlMissing}
            className="mt-3.5 flex h-9 w-full cursor-pointer items-center justify-center gap-2 rounded-md border-none bg-primary font-sans text-[13px] font-medium text-primary-foreground hover:opacity-85 disabled:opacity-50"
          >
            {saving ? "Adding…" : `Add ${picked.name}`}
          </button>

          <div className="mt-4 flex items-center gap-2">
            <button
              type="button"
              onClick={reset}
              className="flex h-[30px] cursor-pointer items-center gap-1.5 rounded-md border-none bg-transparent px-2.5 font-sans text-[12.5px] font-medium text-muted-foreground hover:bg-accent hover:text-accent-foreground"
            >
              <ArrowLeft aria-hidden size={12} strokeWidth={2} />
              Back
            </button>
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
