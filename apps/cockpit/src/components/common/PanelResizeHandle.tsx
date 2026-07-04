import { useRef } from "react";

/** Thin drag handle for panel resizing. Emits raw pointer deltas; the caller
 *  owns clamping and persistence. */
export function PanelResizeHandle({
  direction,
  onDelta,
  className = "",
}: {
  direction: "x" | "y";
  onDelta: (delta: number) => void;
  className?: string;
}) {
  const last = useRef(0);
  return (
    // biome-ignore lint/a11y/useFocusableInteractive: pointer-drag only; keyboard resize is out of scope for this handle
    // biome-ignore lint/a11y/useSemanticElements: <hr> can't carry pointer handlers or an orientation-aware cursor
    <div
      // biome-ignore lint/a11y/useAriaPropsForRole: no meaningful numeric value to report (raw pixel deltas, not a range)
      role="separator"
      aria-orientation={direction === "x" ? "vertical" : "horizontal"}
      onPointerDown={(e) => {
        last.current = direction === "x" ? e.clientX : e.clientY;
        e.currentTarget.setPointerCapture(e.pointerId);
      }}
      onPointerMove={(e) => {
        if (!e.currentTarget.hasPointerCapture(e.pointerId)) return;
        const pos = direction === "x" ? e.clientX : e.clientY;
        onDelta(pos - last.current);
        last.current = pos;
      }}
      onPointerUp={(e) => e.currentTarget.releasePointerCapture(e.pointerId)}
      className={`shrink-0 transition-colors hover:bg-primary/40 active:bg-primary/60 ${
        direction === "x" ? "w-[5px] cursor-col-resize" : "h-[5px] cursor-row-resize"
      } ${className}`}
    />
  );
}
