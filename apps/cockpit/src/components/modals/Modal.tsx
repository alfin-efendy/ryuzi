import { useEffect, type ReactNode } from "react";

// Full-window modal scrim + centered panel per the design's dialog pattern.
export function Modal({ onClose, width, children }: { onClose: () => void; width: number; children: ReactNode }) {
  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if (e.key === "Escape") onClose();
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [onClose]);
  return (
    <div onClick={onClose} className="fixed inset-0 z-[60] flex items-center justify-center bg-black/50" role="presentation">
      <div
        onClick={(e) => e.stopPropagation()}
        className="rounded-xl border border-border bg-popover p-[22px] text-popover-foreground shadow-2xl"
        style={{ width }}
        role="dialog"
      >
        {children}
      </div>
    </div>
  );
}
