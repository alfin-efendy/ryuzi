import { useEffect, useState } from "react";
import { AppWindow, ChevronDown, Code2, Folder, FolderOpen, SquareTerminal, type LucideIcon } from "lucide-react";
import { toast } from "sonner";
import { Button, MenuPanel, MenuPanelItem, MenuPanelSection } from "@ryuzi/ui";
import { commands, type OpenTarget } from "@/bindings";

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
export function OpenInMenu({ sessionPk }: { sessionPk: string }) {
  const [targets, setTargets] = useState<OpenTarget[]>([]);
  const [open, setOpen] = useState(false);

  useEffect(() => {
    let cancelled = false;
    void commands.listOpenTargets().then((list) => {
      if (!cancelled) setTargets(list);
    });
    return () => {
      cancelled = true;
    };
  }, []);

  const openIn = async (targetId: string) => {
    setOpen(false);
    const res = await commands.openIn(sessionPk, targetId);
    if (res.status === "error") toast.error("Couldn't open: " + res.error.message);
  };

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
