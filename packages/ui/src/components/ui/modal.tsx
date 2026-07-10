import { useEffect, type ReactNode } from "react";
import { createPortal } from "react-dom";

import { cn } from "../../lib/utils";

// Full-window modal scrim + centered panel per the design's dialog pattern.
// Rendered through a portal to <body>: ancestors with backdrop-filter (e.g.
// the app shell's `.acrylic-main`) create containing blocks for `fixed`
// descendants, which would confine the scrim to the content area instead of
// covering the whole window.
function Modal({ onClose, width, className, children }: { onClose: () => void; width: number; className?: string; children: ReactNode }) {
  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if (e.key === "Escape") onClose();
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [onClose]);
  return createPortal(
    // biome-ignore lint/a11y/noStaticElementInteractions: scrim click-to-dismiss; Escape is handled globally above
    <div onClick={onClose} className="fixed inset-0 z-[60] flex items-center justify-center bg-black/50" role="presentation">
      {/* biome-ignore lint/a11y/useKeyWithClickEvents: click handler only swallows bubbling so the scrim doesn't dismiss */}
      <div
        onClick={(e) => e.stopPropagation()}
        className={cn("rounded-xl border border-border bg-popover p-[22px] text-popover-foreground shadow-2xl", className)}
        style={{ width }}
        role="dialog"
      >
        {children}
      </div>
    </div>,
    document.body,
  );
}

// Right-aligned action row at the bottom of a Modal body. Insert a
// `<div className="flex-1" />` child to push leading actions (e.g. Back) left.
function ModalFooter({ className, children }: { className?: string; children: ReactNode }) {
  return <div className={cn("mt-[22px] flex items-center justify-end gap-2", className)}>{children}</div>;
}

export { Modal, ModalFooter };
