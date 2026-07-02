// packages/ui/src/components/ui/resizable.tsx
// Thin shadcn-style wrapper over react-resizable-panels v4 (Group/Panel/Separator API).
import {
  Group,
  Panel,
  Separator,
  useDefaultLayout,
  type GroupProps,
  type PanelProps,
  type SeparatorProps,
} from "react-resizable-panels";

import { cn } from "../../lib/utils";

function ResizableGroup({ className, ...props }: GroupProps) {
  return <Group data-slot="resizable-group" className={cn("flex h-full w-full", className)} {...props} />;
}

function ResizablePanel(props: PanelProps) {
  return <Panel data-slot="resizable-panel" {...props} />;
}

function ResizableSeparator({ className, ...props }: SeparatorProps) {
  return (
    <Separator
      data-slot="resizable-separator"
      className={cn(
        "w-px shrink-0 bg-border outline-none transition-colors hover:bg-primary/50 focus-visible:bg-primary data-[separator=active]:bg-primary",
        className,
      )}
      {...props}
    />
  );
}

export { ResizableGroup, ResizablePanel, ResizableSeparator, useDefaultLayout };
export type { PanelImperativeHandle } from "react-resizable-panels";
