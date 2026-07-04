import { useEffect, useRef } from "react";
import { Terminal } from "@xterm/xterm";
import { FitAddon } from "@xterm/addon-fit";
import "@xterm/xterm/css/xterm.css";
import { commands, events } from "@/bindings";

// A real interactive shell in the session's worktree, rendered with xterm and
// backed by the engine's portable-pty terminal (ConPTY on Windows).
export function TerminalPane({ sessionPk, className }: { sessionPk: string; className?: string }) {
  const hostRef = useRef<HTMLDivElement>(null);

  useEffect(() => {
    const host = hostRef.current;
    if (!host) return;

    const style = getComputedStyle(document.documentElement);
    const term = new Terminal({
      fontSize: 12,
      fontFamily: '"Geist Mono Variable", ui-monospace, monospace',
      cursorBlink: true,
      allowTransparency: true,
      theme: {
        background: "rgba(0,0,0,0)",
        foreground: style.getPropertyValue("--code-foreground").trim() || undefined,
      },
    });
    const fit = new FitAddon();
    term.loadAddon(fit);
    term.open(host);
    fit.fit();

    let termId: string | null = null;
    let disposed = false;
    const unlisteners: (() => void)[] = [];

    const boot = async () => {
      const res = await commands.termOpen(sessionPk, term.cols, term.rows);
      if (res.status === "error") {
        term.writeln(`\x1b[31m${res.error.message}\x1b[0m`);
        return;
      }
      if (disposed) {
        void commands.termClose(res.data);
        return;
      }
      termId = res.data;
      unlisteners.push(
        await events.termOutputMsg.listen((e) => {
          if (e.payload.id === termId) term.write(e.payload.data);
        }),
        await events.termExitMsg.listen((e) => {
          if (e.payload.id === termId) term.write("\r\n\x1b[90m[process exited]\x1b[0m\r\n");
        }),
      );
    };
    void boot();

    const onData = term.onData((data) => {
      if (termId) void commands.termInput(termId, data);
    });
    const observer = new ResizeObserver(() => {
      fit.fit();
      if (termId) void commands.termResize(termId, term.cols, term.rows);
    });
    observer.observe(host);

    return () => {
      disposed = true;
      observer.disconnect();
      onData.dispose();
      for (const un of unlisteners) un();
      if (termId) void commands.termClose(termId);
      term.dispose();
    };
  }, [sessionPk]);

  return <div ref={hostRef} className={`min-h-0 px-3 py-2 ${className ?? ""}`} />;
}
