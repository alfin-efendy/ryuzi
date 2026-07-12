import { useEffect, useState } from "react";
import { AppWindow, ChevronDown, Code2, Folder, FolderOpen, SquareTerminal, type LucideIcon } from "lucide-react";
import { toast } from "sonner";
import { Button, MenuPanel, MenuPanelItem, MenuPanelSection } from "@ryuzi/ui";
import { commands, type OpenTarget } from "@/bindings";
import { LOCAL_RUNNER } from "@/lib/session-key";

const targetIcon: Record<string, LucideIcon> = {
  explorer: FolderOpen,
  finder: FolderOpen,
  files: FolderOpen,
  terminal: SquareTerminal,
  "git-bash": SquareTerminal,
  wsl: SquareTerminal,
  vscode: Code2,
  cursor: Code2,
};

/** Header dropdown: open the session workdir in a detected external app. */
export function OpenInMenu({ runnerId, sessionPk }: { runnerId: string; sessionPk: string }) {
  const [targets, setTargets] = useState<OpenTarget[]>([]);
  const [open, setOpen] = useState(false);
  // Every target (Explorer/Finder, a terminal, VS Code, ...) is a locally
  // installed app operating on a local path — none of them can reach a
  // remote runner's workdir, so the whole menu is gated off for remote.
  const isRemote = runnerId !== LOCAL_RUNNER;

  useEffect(() => {
    if (isRemote) return;
    let cancelled = false;
    void commands.listOpenTargets().then((list) => {
      if (!cancelled) setTargets(list);
    });
    return () => {
      cancelled = true;
    };
  }, [isRemote]);

  const openIn = async (targetId: string) => {
    setOpen(false);
    const res = await commands.openIn(runnerId, sessionPk, targetId);
    if (res.status === "error") toast.error("Couldn't open: " + res.error.message);
  };

  // A disabled Button has pointer-events-none, so a native title attribute on
  // it never fires a hover tooltip — wrap in a span (which still receives
  // hover) to carry the "why disabled" reason; the Button keeps its normal
  // title so it still has a stable accessible name. Rendered regardless of
  // `targets` so a remote session still gets a visible (disabled) trigger
  // instead of the menu silently vanishing.
  if (isRemote) {
    return (
      <span title="Not available for sessions on a remote runner">
        <Button variant="ghost" size="icon-sm" title="Open in…" disabled className="text-muted-foreground">
          <Folder aria-hidden size={15} strokeWidth={2} className="size-[15px]" />
          <ChevronDown aria-hidden size={9} strokeWidth={2} className="size-[9px]" />
        </Button>
      </span>
    );
  }

  if (targets.length === 0) return null;
  return (
    <div className="relative">
      <Button
        variant="ghost"
        size="icon-sm"
        title="Open in…"
        aria-expanded={open}
        onClick={() => setOpen((v) => !v)}
        className={open ? "bg-accent text-accent-foreground" : "text-muted-foreground"}
      >
        <Folder aria-hidden size={15} strokeWidth={2} className="size-[15px]" />
        <ChevronDown aria-hidden size={9} strokeWidth={2} className="size-[9px]" />
      </Button>
      {open && (
        <MenuPanel onClose={() => setOpen(false)} className="right-0 top-9 z-50 w-[220px]">
          <MenuPanelSection>Open in</MenuPanelSection>
          {targets.map((t) => {
            const Icon = targetIcon[t.id] ?? AppWindow;
            return (
              <MenuPanelItem key={t.id} onClick={() => void openIn(t.id)} className="font-medium">
                <Icon aria-hidden size={13} strokeWidth={2} className="size-[13px] text-muted-foreground" />
                {t.name}
              </MenuPanelItem>
            );
          })}
        </MenuPanel>
      )}
    </div>
  );
}
