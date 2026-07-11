import type * as React from "react";
import { cn } from "../../lib/utils";

function Field({ className, ...props }: React.ComponentProps<"div">) {
  return <div data-slot="field" className={cn("flex min-w-0 gap-3", className)} {...props} />;
}

function FieldLabel({ className, ...props }: React.ComponentProps<"label">) {
  // biome-ignore lint/a11y/noLabelWithoutControl: controls are associated through `htmlFor` or nested by the caller.
  return <label data-slot="field-label" className={cn("font-medium", className)} {...props} />;
}

function FieldContent({ className, ...props }: React.ComponentProps<"span">) {
  return <span data-slot="field-content" className={cn("min-w-0 flex-1", className)} {...props} />;
}

function FieldDescription({ className, ...props }: React.ComponentProps<"span">) {
  return (
    <span data-slot="field-description" className={cn("mt-0.5 block text-xs font-normal text-muted-foreground", className)} {...props} />
  );
}

export { Field, FieldContent, FieldDescription, FieldLabel };
