import { useState } from "react";
import { Button, Textarea } from "@ryuzi/ui";
import { HandHelping } from "lucide-react";
import { useStore } from "@/store";

/** An `orch_block` speaker row: a worker paused an orchestrated subtask and
 *  is waiting on a human answer. Renders in place of a normal SpeakerBubble
 *  (see Transcript's `speaker` group render) with an inline answer composer
 *  that resolves the block via `orchAnswerBlock`. */
export function BlockCard({ taskId, question, speaker }: { taskId: string; question: string; speaker: string }) {
  const answerBlock = useStore((s) => s.orchAnswerBlock);
  const [answer, setAnswer] = useState("");
  const [sent, setSent] = useState(false);
  const [sending, setSending] = useState(false);

  const submit = async () => {
    const trimmed = answer.trim();
    if (!trimmed || sending) return;
    setSending(true);
    await answerBlock(taskId, trimmed);
    setSending(false);
    setSent(true);
  };

  return (
    <div className="max-w-[82%] rounded-lg border border-amber-500/40 bg-amber-500/5 p-3">
      <div className="mb-1.5 flex items-center gap-1.5 text-[11px] font-medium text-amber-600 dark:text-amber-400">
        <HandHelping aria-hidden size={13} strokeWidth={2} className="size-[13px]" />
        {speaker} needs your input
      </div>
      <div className="mb-2 text-[13.5px] leading-relaxed text-foreground">{question}</div>
      {sent ? (
        <div className="text-[12px] text-muted-foreground">Answer sent — the worker will resume.</div>
      ) : (
        <div className="flex flex-col gap-2">
          <Textarea
            aria-label="Answer the worker"
            value={answer}
            onChange={(e) => setAnswer(e.target.value)}
            placeholder="Answer the worker…"
            rows={2}
          />
          <Button size="sm" disabled={!answer.trim() || sending} onClick={() => void submit()} className="self-end">
            Send answer
          </Button>
        </div>
      )}
    </div>
  );
}
