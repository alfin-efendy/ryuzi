import type * as React from "react";
import { ChevronDown } from "lucide-react";

import { cn } from "../../lib/utils";

// Native <select> styled to match Input — zero behavior change from a bare
// select element, which matters inside the Tauri webview (OS-native popup).
function NativeSelect({ className, children, ...props }: React.ComponentProps<"select">) {
  return (
    <span className={cn("relative inline-flex w-full", className)}>
      <select
        data-slot="native-select"
        className="h-8 w-full min-w-0 cursor-pointer appearance-none rounded-lg border border-input bg-transparent px-2.5 py-1 pr-8 text-base transition-colors outline-none focus-visible:border-ring focus-visible:ring-3 focus-visible:ring-ring/50 disabled:pointer-events-none disabled:cursor-not-allowed disabled:opacity-50 md:text-sm dark:bg-input/30"
        {...props}
      >
        {children}
      </select>
      <ChevronDown aria-hidden className="pointer-events-none absolute top-1/2 right-2.5 size-4 -translate-y-1/2 text-muted-foreground" />
    </span>
  );
}

export { NativeSelect };
