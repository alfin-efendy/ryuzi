import { useState } from "react";
import { ArrowLeft } from "lucide-react";
import { useConnections } from "@/store-connections";
import type { CatalogEntry } from "@/bindings";
import { Chip, Pill } from "@/components/common/bits";
import { Button, FormField, Input, Modal, ModalFooter } from "@ryuzi/ui";

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
                      {ci.format === "anthropic" ? "Anthropic-compatible" : "OpenAI-compatible"}
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

          <ModalFooter className="mt-4">
            <Button variant="ghost" onClick={reset} className="text-muted-foreground">
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
