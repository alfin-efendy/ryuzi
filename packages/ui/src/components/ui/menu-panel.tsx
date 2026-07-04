import { useEffect, useRef, type ReactNode } from "react";
import { Check } from "lucide-react";

import { cn } from "../../lib/utils";

function useClickOutside(onClose: () => void) {
  const ref = useRef<HTMLDivElement>(null);
  useEffect(() => {
    const handler = (e: MouseEvent) => {
      if (ref.current && !ref.current.contains(e.target as Node)) onClose();
    };
    // Defer so the click that opened the menu doesn't immediately close it.
    const t = setTimeout(() => document.addEventListener("mousedown", handler), 0);
    return () => {
      clearTimeout(t);
      document.removeEventListener("mousedown", handler);
    };
  }, [onClose]);
  return ref;
}

type MenuPanelProps = {
  onClose: () => void;
  className?: string;
  children: ReactNode;
};

// Absolutely-positioned popover panel; the parent supplies position classes.
// Statically placed counterpart to the anchored Menu — use Menu when a
// trigger-anchored dropdown fits; use MenuPanel inside custom containers.
function MenuPanel({ onClose, className, children }: MenuPanelProps) {
  const ref = useClickOutside(onClose);
  return (
    <div
      ref={ref}
      className={cn("absolute z-50 rounded-lg border border-border bg-popover p-[5px] text-popover-foreground shadow-lg", className)}
    >
      {children}
    </div>
  );
}

function MenuPanelSection({ children }: { children: ReactNode }) {
  return (
    <div className="px-2.5 pb-[5px] pt-[7px] text-[11px] font-semibold uppercase tracking-[0.04em] text-muted-foreground">{children}</div>
  );
}

type MenuPanelItemProps = {
  onClick?: () => void;
  selected?: boolean;
  className?: string;
  children: ReactNode;
};

function MenuPanelItem({ onClick, selected, className, children }: MenuPanelItemProps) {
  return (
    <button
      type="button"
      onClick={onClick}
      className={cn(
        "flex w-full cursor-pointer items-center gap-2.5 rounded-md border-none bg-transparent px-2.5 py-2 text-left font-sans text-[13px] text-popover-foreground hover:bg-accent",
        className,
      )}
    >
      {children}
      {selected && <Check aria-hidden size={14} strokeWidth={2.5} className="shrink-0" />}
    </button>
  );
}

function MenuPanelSeparator() {
  return <div className="my-1 border-t border-border" />;
}

export { MenuPanel, MenuPanelSection, MenuPanelItem, MenuPanelSeparator, useClickOutside };
