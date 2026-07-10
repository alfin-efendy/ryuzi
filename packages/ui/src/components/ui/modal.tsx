import { Dialog } from "@base-ui/react/dialog";
import { X } from "lucide-react";
import * as React from "react";

import { cn } from "../../lib/utils";
import { Button } from "./button";

type ModalContextValue = { busy: boolean };
const ModalContext = React.createContext<ModalContextValue>({ busy: false });

type ModalProps = {
  onClose: () => void;
  width: number;
  busy?: boolean;
  className?: string;
  initialFocus?: React.RefObject<HTMLElement | null>;
  children: React.ReactNode;
};

function ModalInitialFocus({ target, children }: { target?: React.RefObject<HTMLElement | null>; children: React.ReactNode }) {
  React.useLayoutEffect(() => {
    target?.current?.focus();
  }, [target]);
  return children;
}

function Modal({ onClose, width, busy = false, className, initialFocus, children }: ModalProps) {
  const finalFocus = React.useRef<HTMLElement | null>(null);
  React.useLayoutEffect(() => {
    finalFocus.current = document.activeElement instanceof HTMLElement ? document.activeElement : null;
  }, []);

  return (
    <Dialog.Root
      open
      onOpenChange={(open) => {
        if (!open && !busy) onClose();
      }}
    >
      <Dialog.Portal>
        <Dialog.Backdrop data-slot="modal-backdrop" className="fixed inset-0 z-[60] bg-black/50" />
        <Dialog.Viewport className="fixed inset-0 z-[60] flex items-center justify-center overflow-y-auto p-4">
          <ModalInitialFocus target={initialFocus}>
            <Dialog.Popup
              data-slot="modal"
              initialFocus={initialFocus}
              finalFocus={finalFocus}
              aria-busy={busy || undefined}
              className={cn(
                "max-h-[calc(100vh-2rem)] overflow-y-auto rounded-xl border border-border bg-popover p-[22px] text-popover-foreground shadow-2xl outline-none",
                className,
              )}
              style={{ width }}
            >
              <ModalContext.Provider value={{ busy }}>{children}</ModalContext.Provider>
            </Dialog.Popup>
          </ModalInitialFocus>
        </Dialog.Viewport>
      </Dialog.Portal>
    </Dialog.Root>
  );
}

type ModalHeaderProps = {
  title: React.ReactNode;
  description?: React.ReactNode;
  leading?: React.ReactNode;
  className?: string;
};

function ModalHeader({ title, description, leading, className }: ModalHeaderProps) {
  const { busy } = React.useContext(ModalContext);
  return (
    <div data-slot="modal-header" className={cn("relative flex min-w-0 items-start gap-2.5 pr-9", className)}>
      {leading}
      <div className="min-w-0 flex-1">
        <Dialog.Title className="text-[15px] font-semibold tracking-[-0.01em]">{title}</Dialog.Title>
        {description !== undefined && (
          <Dialog.Description className="mt-1 text-[12.5px] leading-[1.5] text-muted-foreground">{description}</Dialog.Description>
        )}
      </div>
      <Dialog.Close
        aria-label="Close"
        disabled={busy}
        render={<Button type="button" variant="ghost" size="icon-sm" className="absolute -right-1 -top-1 text-muted-foreground" />}
      >
        <X aria-hidden />
      </Dialog.Close>
    </div>
  );
}

function ModalBody({ className, children }: { className?: string; children: React.ReactNode }) {
  return (
    <div data-slot="modal-body" className={cn("mt-[18px]", className)}>
      {children}
    </div>
  );
}

function ModalFooter({ className, children }: { className?: string; children?: React.ReactNode }) {
  return (
    <div data-slot="modal-footer" className={cn("mt-[22px] flex items-center justify-end gap-2 border-t border-border pt-4", className)}>
      {children}
    </div>
  );
}

export { Modal, ModalBody, ModalFooter, ModalHeader };
export type { ModalHeaderProps, ModalProps };
