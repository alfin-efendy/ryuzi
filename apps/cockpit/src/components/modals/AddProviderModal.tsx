import { useState } from "react";
import { ArrowLeft, ArrowUpRight } from "lucide-react";
import { PROVIDER_CATALOG, PROVIDERS } from "@/fixtures";
import { Chip } from "@/components/common/bits";
import { Segmented } from "@/components/common/Segmented";
import { Modal } from "./Modal";

type CatalogEntry = (typeof PROVIDER_CATALOG)[number];
type Method = "oauth" | "key";

const METHODS: { id: Method; label: string }[] = [
  { id: "oauth", label: "OAuth sign-in" },
  { id: "key", label: "API key" },
];

const cancelBtn =
  "cursor-pointer rounded-md border border-border bg-transparent font-sans text-[12.5px] font-medium text-foreground hover:bg-accent";

// Two-step connect flow: pick a provider from the catalog, then sign in with
// OAuth or paste an API key. Also reused as the "Add account" dialog on the
// provider detail screen.
export function AddProviderModal({ open, onClose }: { open: boolean; onClose: () => void }) {
  const [picked, setPicked] = useState<CatalogEntry | null>(null);
  const [method, setMethod] = useState<Method>("oauth");
  const [key, setKey] = useState("");
  if (!open) return null;

  const reset = () => {
    setPicked(null);
    setMethod("oauth");
    setKey("");
  };
  const close = () => {
    reset();
    onClose();
  };
  const keyReady = key.trim().length > 0;

  return (
    <Modal onClose={close} width={480}>
      {picked === null ? (
        <>
          <div className="text-[15px] font-semibold tracking-[-0.01em]">Connect provider</div>
          <p className="mb-4 mt-1 text-[12.5px] text-muted-foreground">Pick a provider, then sign in or paste an API key.</p>
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
                    {PROVIDERS.some((p) => p.id === ci.id) && (
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
            <Chip initial={picked.initial} color={picked.color} size={36} />
            <div className="min-w-0 flex-1">
              <div className="text-[15px] font-semibold tracking-[-0.01em]">Connect provider</div>
              <div className="text-xs text-muted-foreground">{picked.name}</div>
            </div>
          </div>

          <div className="mb-3.5 mt-4">
            <Segmented options={METHODS} value={method} onChange={setMethod} />
          </div>

          {method === "oauth" ? (
            <>
              <p className="m-0 mb-3.5 text-[12.5px] leading-[1.55] text-muted-foreground">
                A browser window opens for the provider&#8217;s sign-in. Cockpit stores the refresh token in the system keychain — usage
                quotas are read from the account automatically.
              </p>
              <button
                type="button"
                className="flex h-9 w-full cursor-pointer items-center justify-center gap-2 rounded-md border-none bg-primary font-sans text-[13px] font-medium text-primary-foreground hover:opacity-85"
              >
                Continue with {picked.name} sign-in
                <ArrowUpRight aria-hidden size={13} strokeWidth={2} />
              </button>
            </>
          ) : (
            <>
              <div className="mb-3.5 flex flex-col gap-1.5">
                <span className="text-xs font-semibold">API key</span>
                <input
                  value={key}
                  onChange={(e) => setKey(e.target.value)}
                  placeholder="sk-…"
                  className="h-9 rounded-md border border-input bg-background px-3 font-mono text-[12.5px] text-foreground"
                />
                <span className="text-[11.5px] text-muted-foreground">
                  Stored in the system keychain. Cost tracking uses the provider&#8217;s usage API.
                </span>
              </div>
              <button
                type="button"
                onClick={() => {
                  if (keyReady) close();
                }}
                className={`flex h-9 w-full items-center justify-center gap-2 rounded-md border-none bg-primary font-sans text-[13px] font-medium text-primary-foreground ${
                  keyReady ? "cursor-pointer hover:opacity-85" : "cursor-default opacity-45"
                }`}
              >
                Verify &amp; add
              </button>
            </>
          )}

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
