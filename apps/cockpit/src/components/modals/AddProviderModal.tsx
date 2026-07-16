import { useMemo } from "react";
import { Plus } from "lucide-react";
import type { CatalogEntry } from "@/bindings";
import { Chip } from "@/components/common/bits";
import { Button, Modal, ModalBody, ModalHeader } from "@ryuzi/ui";

/** Pure: the family heads in the catalog that are not yet installed. */
export function installableFamilies(catalog: CatalogEntry[], installed: string[]): CatalogEntry[] {
  const heads = catalog.filter((entry) => entry.id === entry.family);
  return heads.filter((entry) => !installed.includes(entry.id));
}

export function AddProviderModal({
  open,
  onClose,
  catalog,
  installed,
  onInstall,
}: {
  open: boolean;
  onClose: () => void;
  catalog: CatalogEntry[];
  installed: string[];
  onInstall: (family: string) => Promise<boolean>;
}) {
  const options = useMemo(() => installableFamilies(catalog, installed), [catalog, installed]);
  if (!open) return null;
  return (
    <Modal onClose={onClose} width={420}>
      <ModalHeader title="Add provider" />
      <ModalBody>
        {options.length === 0 ? (
          <div className="py-6 text-center text-[13px] text-muted-foreground">Every provider is already installed.</div>
        ) : (
          <div className="flex flex-col">
            {options.map((entry) => (
              <div key={entry.id} className="flex items-center gap-3 border-b border-border py-2.5 last:border-b-0">
                <Chip initial={entry.initial} color={entry.color} size={30} />
                <span className="min-w-0 flex-1 truncate text-sm font-medium text-foreground">{entry.name}</span>
                <Button size="sm" aria-label={`Install ${entry.name}`} onClick={() => void onInstall(entry.id).then((ok) => ok && onClose())}>
                  <Plus aria-hidden size={13} className="mr-1.5" />
                  Install
                </Button>
              </div>
            ))}
          </div>
        )}
      </ModalBody>
    </Modal>
  );
}
