import { useEffect } from "react";

/** Suppress the default WebView context menu (Reload/Inspect) in release builds; keep it in dev. */
export function shouldSuppressContextMenu(isDev: boolean): boolean {
  return !isDev;
}

export function useDisableContextMenu(): void {
  useEffect(() => {
    if (!shouldSuppressContextMenu(import.meta.env.DEV)) return;
    const handler = (e: MouseEvent) => e.preventDefault();
    document.addEventListener("contextmenu", handler);
    return () => document.removeEventListener("contextmenu", handler);
  }, []);
}
