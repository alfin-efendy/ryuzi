import { memo, useEffect, useMemo, useRef, type ReactNode } from "react";
import { closeDanglingFence, groupRows, type Row } from "@/lib/transcript";
import { StatusDot } from "@/components/common/bits";
import { Markdown } from "./Markdown";
import { ThoughtBlock } from "./ThoughtBlock";
import { ActivityCluster } from "./ToolChip";

function UserBubble({ text }: { text: string }) {
  return (
    <div className="flex flex-col">
      <div className="max-w-[70%] self-end whitespace-pre-wrap rounded-xl bg-secondary px-3.5 py-2.5 text-[13.5px] leading-[1.55] text-secondary-foreground">
        {text}
      </div>
    </div>
  );
}

function ErrorRow({ text }: { text: string }) {
  return (
    <div className="flex flex-col">
      <div className="flex max-w-fit items-center gap-2 rounded-md border border-destructive/40 bg-destructive/10 px-3 py-[7px] text-xs text-destructive">
        {text}
      </div>
    </div>
  );
}

// memo: completed turns never re-render while the streaming tail grows.
const AgentTurn = memo(function AgentTurn({
  markdown,
  agentName,
  agentColor,
}: {
  markdown: string;
  agentName: string;
  agentColor: string;
}) {
  return (
    <div className="flex max-w-[82%] flex-col text-[13.5px] leading-relaxed text-foreground">
      <div className="mb-1 flex items-center gap-1.5 text-[11.5px] font-semibold text-muted-foreground">
        <StatusDot color={agentColor} />
        {agentName}
      </div>
      <Markdown text={markdown} />
    </div>
  );
});

function WorkingPulse({ color }: { color: string }) {
  return (
    <div className="flex items-center gap-2 text-[12.5px] text-muted-foreground">
      <span className="h-2 w-2 rounded-full" style={{ background: color, animation: "relay-pulse 1.2s ease-in-out infinite" }} />
      Working…
    </div>
  );
}

export function Transcript({
  rows,
  agentName,
  agentColor,
  running,
  children,
}: {
  rows: Row[];
  agentName: string;
  agentColor: string;
  running: boolean;
  children?: ReactNode;
}) {
  const scrollRef = useRef<HTMLDivElement>(null);
  // Stick to the bottom only while the user is already there; scrolling up
  // to read pauses the auto-scroll until they return to the bottom.
  const stickRef = useRef(true);

  const groups = useMemo(() => groupRows(rows), [rows]);
  const tail = groups[groups.length - 1];
  // Growth signal for the tail group: coalesced text grows by length, an
  // activity cluster grows by item count — either must re-pin the scroll.
  const tailLen =
    tail === undefined
      ? 0
      : tail.type === "agent" || tail.type === "thought"
        ? tail.markdown.length
        : tail.type === "activity"
          ? tail.items.length
          : 0;

  const onScroll = () => {
    const el = scrollRef.current;
    if (el) stickRef.current = el.scrollHeight - el.scrollTop - el.clientHeight < 40;
  };

  // Keyed on group count AND tail text length: with coalescing, a growing turn
  // changes length, not count.
  // biome-ignore lint/correctness/useExhaustiveDependencies: scroll pinning reacts to transcript growth, not identity
  useEffect(() => {
    if (stickRef.current) scrollRef.current?.scrollTo({ top: scrollRef.current.scrollHeight });
  }, [groups.length, tailLen, running]);

  return (
    <div ref={scrollRef} onScroll={onScroll} className="flex min-h-0 flex-1 flex-col gap-3.5 overflow-y-auto px-6 py-5">
      {groups.map((g, i) => {
        const streamingTail = running && i === groups.length - 1;
        switch (g.type) {
          case "user":
            return <UserBubble key={g.key} text={g.text} />;
          case "agent":
            return (
              <AgentTurn
                key={g.key}
                markdown={streamingTail ? closeDanglingFence(g.markdown) : g.markdown}
                agentName={agentName}
                agentColor={agentColor}
              />
            );
          case "thought":
            return <ThoughtBlock key={g.key} markdown={g.markdown} streaming={streamingTail} />;
          case "activity":
            return <ActivityCluster key={g.key} items={g.items} />;
          case "error":
            return <ErrorRow key={g.key} text={g.text} />;
          default:
            return null;
        }
      })}
      {running && <WorkingPulse color={agentColor} />}
      {children}
    </div>
  );
}
