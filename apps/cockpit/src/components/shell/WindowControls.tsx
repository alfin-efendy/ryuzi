// apps/cockpit/src/components/shell/WindowControls.tsx
import { useEffect, useMemo, useState } from "react";
import { getCurrentWindow } from "@tauri-apps/api/window";
import { Button } from "@ryuzi/ui";

// Window chrome must stay pixel-identical: keep the 30x42 footprint,
// rounded-md corners, and the accent/destructive hover colors.
const btn = "h-[30px] w-[42px] rounded-md text-muted-foreground transition-colors";

export function WindowControls() {
  const win = useMemo(() => getCurrentWindow(), []);
  const [max, setMax] = useState(false);
  useEffect(() => {
    win
      .isMaximized()
      .then(setMax)
      .catch(() => {});
    const un = win.onResized(() => {
      win
        .isMaximized()
        .then(setMax)
        .catch(() => {});
    });
    return () => {
      un.then((f) => f()).catch(() => {});
    };
  }, [win]);
  return (
    <div className="flex gap-0.5">
      <Button
        type="button"
        variant="ghost"
        size="icon-sm"
        aria-label="Minimize"
        className={`${btn} hover:bg-accent hover:text-foreground dark:hover:bg-accent`}
        onClick={() => win.minimize()}
      >
        <svg aria-hidden="true" className="size-[11px]" width="11" height="11" viewBox="0 0 12 12">
          <rect x="1.5" y="5.4" width="9" height="1.2" fill="currentColor" />
        </svg>
      </Button>
      <Button
        type="button"
        variant="ghost"
        size="icon-sm"
        aria-label="Maximize"
        className={`${btn} hover:bg-accent hover:text-foreground dark:hover:bg-accent`}
        onClick={() => win.toggleMaximize()}
      >
        {max ? (
          <svg
            aria-hidden="true"
            className="size-[11px]"
            width="11"
            height="11"
            viewBox="0 0 12 12"
            fill="none"
            stroke="currentColor"
            strokeWidth="1.2"
          >
            <rect x="2.4" y="3.4" width="6.2" height="6.2" rx="1" />
            <path d="M4 3.4V2.4h6.2v6.2H9.2" />
          </svg>
        ) : (
          <svg
            aria-hidden="true"
            className="size-[11px]"
            width="11"
            height="11"
            viewBox="0 0 12 12"
            fill="none"
            stroke="currentColor"
            strokeWidth="1.2"
          >
            <rect x="2" y="2" width="8" height="8" rx="1.2" />
          </svg>
        )}
      </Button>
      <Button
        type="button"
        variant="ghost"
        size="icon-sm"
        aria-label="Close"
        className={`${btn} hover:bg-destructive hover:text-destructive-foreground dark:hover:bg-destructive`}
        onClick={() => win.close()}
      >
        <svg
          aria-hidden="true"
          className="size-[11px]"
          width="11"
          height="11"
          viewBox="0 0 12 12"
          fill="none"
          stroke="currentColor"
          strokeWidth="1.3"
          strokeLinecap="round"
        >
          <path d="M3 3l6 6M9 3l-6 6" />
        </svg>
      </Button>
    </div>
  );
}
