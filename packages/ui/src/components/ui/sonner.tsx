import type { CSSProperties } from "react";
import { Toaster as SonnerToaster, type ToasterProps } from "sonner";

const Toaster = (props: ToasterProps) => {
  return (
    <SonnerToaster
      className="toaster group"
      style={
        {
          "--normal-bg": "var(--popover)",
          "--normal-text": "var(--popover-foreground)",
          "--normal-border": "var(--border)",
          "--border-radius": "var(--radius)",
        } as CSSProperties
      }
      {...props}
    />
  );
};

export { Toaster };
