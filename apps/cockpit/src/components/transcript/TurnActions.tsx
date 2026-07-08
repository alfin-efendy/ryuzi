import { useState } from "react";
import { Check, Copy, ThumbsDown, ThumbsUp } from "lucide-react";
import { Button } from "@ryuzi/ui";

/** Copy + local-only feedback under a completed turn's final answer. */
export function TurnActions({ markdown }: { markdown: string }) {
  const [copied, setCopied] = useState(false);
  const [feedback, setFeedback] = useState<"up" | "down" | null>(null);
  const copy = () => {
    void navigator.clipboard.writeText(markdown).then(() => {
      setCopied(true);
      setTimeout(() => setCopied(false), 1500);
    });
  };
  const fbClass = (v: "up" | "down") => (feedback === v ? "text-foreground" : "text-muted-foreground");
  return (
    <div className="mt-1 flex items-center gap-0.5">
      <Button variant="ghost" size="icon-xs" title="Copy response" onClick={copy} className="text-muted-foreground">
        {copied ? <Check aria-hidden size={12} strokeWidth={2} /> : <Copy aria-hidden size={12} strokeWidth={2} />}
      </Button>
      <Button
        variant="ghost"
        size="icon-xs"
        title="Good response"
        onClick={() => setFeedback((f) => (f === "up" ? null : "up"))}
        className={fbClass("up")}
      >
        <ThumbsUp aria-hidden size={12} strokeWidth={2} />
      </Button>
      <Button
        variant="ghost"
        size="icon-xs"
        title="Poor response"
        onClick={() => setFeedback((f) => (f === "down" ? null : "down"))}
        className={fbClass("down")}
      >
        <ThumbsDown aria-hidden size={12} strokeWidth={2} />
      </Button>
    </div>
  );
}
