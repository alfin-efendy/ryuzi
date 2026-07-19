import { useEffect, useState } from "react";
import { FolderInput, Pencil, RotateCcw, Trash2 } from "lucide-react";
import { toast } from "sonner";
import { Button, SettingsCard, SettingsCardRow } from "@ryuzi/ui";
import { commands } from "@/bindings";
import { useNav } from "@/store-nav";
import { reviewFileIndex, useDiff } from "@/store-diff";
import { basename, toRepoRelative } from "@/lib/paths";
import { DiffStat } from "@/components/common/bits";
import type { EditCard } from "@/lib/transcript";
import { sessKey } from "@/lib/session-key";
import { ConfirmActionModal } from "@/components/modals/ConfirmActionModal";

const kindIcon = { edit: Pencil, write: Pencil, delete: Trash2, move: FolderInput } as const;
const kindStyle = {
  edit: "bg-amber-500/10 text-amber-700 dark:text-amber-400",
  write: "bg-green-500/10 text-green-700 dark:text-green-400",
  delete: "bg-destructive/10 text-destructive",
  move: "bg-sky-500/10 text-sky-700 dark:text-sky-400",
} as const;

/** Per-file change cards under a turn summary: DiffStat from the shared diff,
 *  Review jumps the right panel to the file, Undo reverts it to HEAD. */
export function FileChangeCards({ runnerId, sessionPk, cards }: { runnerId: string; sessionPk: string; cards: EditCard[] }) {
  const key = sessKey(runnerId, sessionPk);
  const files = useDiff((s) => s.bySession[key]?.files ?? []);
  const hasDiff = useDiff((s) => s.bySession[key] !== undefined);
  const fetchDiff = useDiff((s) => s.fetch);
  const setPendingReview = useDiff((s) => s.setPendingReview);
  const nav = useNav();
  const [confirming, setConfirming] = useState<{ card: EditCard; trigger: HTMLButtonElement } | null>(null);
  const [reverted, setReverted] = useState<Record<string, true>>({});

  // Cards render even when the right panel (the usual fetch trigger) is
  // closed — fetch once so +adds/-dels aren't blank by default.
  useEffect(() => {
    if (cards.length > 0 && !hasDiff) void fetchDiff(runnerId, sessionPk);
  }, [cards.length, hasDiff, runnerId, sessionPk, fetchDiff]);

  const review = (card: EditCard) => {
    setPendingReview({ runnerId, sessionPk, path: card.path });
    nav.setRightOpen(true);
    nav.setRightTab("review");
  };

  const undo = async (card: EditCard) => {
    const wd = await commands.sessionWorkdir(runnerId, sessionPk);
    const rel = toRepoRelative(card.path, wd.status === "ok" ? wd.data : "");
    const res = await commands.revertFile(runnerId, sessionPk, rel);
    if (res.status === "ok") {
      setReverted((cur) => ({ ...cur, [card.path]: true }));
      toast.success(`Reverted ${basename(card.path)}`);
      void fetchDiff(runnerId, sessionPk);
    } else {
      toast.error("Couldn't revert: " + res.error.message);
    }
    return true;
  };

  if (cards.length === 0) return null;
  return (
    <SettingsCard className="max-w-[640px]">
      {cards.map((card) => {
        const Icon = kindIcon[card.kind as keyof typeof kindIcon] ?? Pencil;
        const tone = kindStyle[card.kind as keyof typeof kindStyle] ?? "bg-muted text-muted-foreground";
        const idx = reviewFileIndex(files, card.path);
        const stat = idx >= 0 ? files[idx] : null;
        const isReverted = reverted[card.path] === true;
        return (
          <SettingsCardRow
            key={card.path}
            className={`gap-3 py-2.5 ${card.kind === "delete" ? "bg-destructive/5" : card.kind === "write" ? "bg-green-500/5" : ""}`}
          >
            <div className={`flex size-7 shrink-0 items-center justify-center rounded-md ${tone}`}>
              <Icon aria-hidden size={14} strokeWidth={2} />
            </div>
            <span
              title={card.path}
              className={`min-w-0 flex-1 truncate font-mono text-[12px] ${isReverted ? "line-through opacity-60" : ""}`}
            >
              {card.path}
            </span>
            {stat && !isReverted && <DiffStat add={stat.add} del={stat.del} className="shrink-0 text-[11px]" />}
            {isReverted ? (
              <span className="shrink-0 text-[11.5px] text-muted-foreground">Reverted</span>
            ) : (
              <div className="flex shrink-0 items-center gap-1">
                <Button variant="outline" size="xs" onClick={() => review(card)}>
                  Review
                </Button>
                <Button
                  variant="ghost"
                  size="xs"
                  onClick={(event) => setConfirming({ card, trigger: event.currentTarget })}
                  className="text-muted-foreground"
                >
                  <RotateCcw aria-hidden size={11} strokeWidth={2} className="size-[11px]" />
                  Undo
                </Button>
              </div>
            )}
          </SettingsCardRow>
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
    </SettingsCard>
  );
}
