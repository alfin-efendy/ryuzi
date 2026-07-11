import { useEffect, useState } from "react";
import { FolderInput, Pencil, RotateCcw, Trash2 } from "lucide-react";
import { toast } from "sonner";
import { Button } from "@ryuzi/ui";
import { commands } from "@/bindings";
import { useNav } from "@/store-nav";
import { reviewFileIndex, useDiff } from "@/store-diff";
import { basename, toRepoRelative } from "@/lib/paths";
import { DiffStat } from "@/components/common/bits";
import type { EditCard } from "@/lib/transcript";
import { ConfirmActionModal } from "@/components/modals/ConfirmActionModal";

const kindIcon = { edit: Pencil, write: Pencil, delete: Trash2, move: FolderInput } as const;

/** Per-file change cards under a turn summary: DiffStat from the shared diff,
 *  Review jumps the right panel to the file, Undo reverts it to HEAD. */
export function FileChangeCards({ sessionPk, cards }: { sessionPk: string; cards: EditCard[] }) {
  const files = useDiff((s) => s.bySession[sessionPk]?.files ?? []);
  const hasDiff = useDiff((s) => s.bySession[sessionPk] !== undefined);
  const fetchDiff = useDiff((s) => s.fetch);
  const setPendingReview = useDiff((s) => s.setPendingReview);
  const nav = useNav();
  const [confirming, setConfirming] = useState<{ card: EditCard; trigger: HTMLButtonElement } | null>(null);
  const [reverted, setReverted] = useState<Record<string, true>>({});

  // Cards render even when the right panel (the usual fetch trigger) is
  // closed — fetch once so +adds/-dels aren't blank by default.
  useEffect(() => {
    if (cards.length > 0 && !hasDiff) void fetchDiff(sessionPk);
  }, [cards.length, hasDiff, sessionPk, fetchDiff]);

  const review = (card: EditCard) => {
    setPendingReview({ sessionPk, path: card.path });
    nav.setRightOpen(true);
    nav.setRightTab("review");
  };

  const undo = async (card: EditCard) => {
    const wd = await commands.sessionWorkdir(sessionPk);
    const rel = toRepoRelative(card.path, wd.status === "ok" ? wd.data : "");
    const res = await commands.revertFile(sessionPk, rel);
    if (res.status === "ok") {
      setReverted((cur) => ({ ...cur, [card.path]: true }));
      toast.success(`Reverted ${basename(card.path)}`);
      void fetchDiff(sessionPk);
    } else {
      toast.error("Couldn't revert: " + res.error.message);
    }
    return true;
  };

  if (cards.length === 0) return null;
  return (
    <div className="flex flex-col gap-1.5">
      {cards.map((card) => {
        const Icon = kindIcon[card.kind as keyof typeof kindIcon] ?? Pencil;
        const idx = reviewFileIndex(files, card.path);
        const stat = idx >= 0 ? files[idx] : null;
        const isReverted = reverted[card.path] === true;
        return (
          <div key={card.path} className="acrylic-panel flex items-center gap-2.5 rounded-lg border border-border px-3 py-2 text-[12.5px]">
            <Icon aria-hidden size={13} strokeWidth={2} className="shrink-0 text-muted-foreground" />
            <span title={card.path} className={`min-w-0 flex-1 truncate font-mono ${isReverted ? "line-through opacity-60" : ""}`}>
              {card.path}
            </span>
            {stat && !isReverted && <DiffStat add={stat.add} del={stat.del} className="shrink-0 text-[11px]" />}
            {isReverted ? (
              <span className="shrink-0 text-[11.5px] text-muted-foreground">Reverted</span>
            ) : (
              <>
                <Button variant="ghost" size="xs" onClick={() => review(card)} className="shrink-0 font-medium">
                  Review
                </Button>
                <Button
                  variant="ghost"
                  size="xs"
                  onClick={(event) => setConfirming({ card, trigger: event.currentTarget })}
                  className="shrink-0 font-medium text-muted-foreground"
                >
                  <RotateCcw aria-hidden size={11} strokeWidth={2} className="size-[11px]" />
                  Undo
                </Button>
              </>
            )}
          </div>
        );
      })}
      <ConfirmActionModal
        open={confirming !== null}
        title={confirming ? `Revert ${basename(confirming.card.path)}?` : "Confirm revert"}
        description={
          confirming ? (
            <>
              This restores <span className="font-mono">{confirming.card.path}</span> to its last committed state (new files are deleted).
              Sessions without a worktree modify your real checkout.
            </>
          ) : null
        }
        confirmLabel="Revert"
        busyLabel="Reverting…"
        trigger={confirming?.trigger ?? null}
        onClose={() => setConfirming(null)}
        onConfirm={() => (confirming ? undo(confirming.card) : Promise.resolve(false))}
      />
    </div>
  );
}
