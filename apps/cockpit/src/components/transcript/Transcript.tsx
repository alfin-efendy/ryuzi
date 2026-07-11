import { memo, useEffect, useMemo, useRef, useState, type ReactNode } from "react";
import { convertFileSrc } from "@tauri-apps/api/core";
import { AudioLines, Paperclip } from "lucide-react";
import { buildTranscript, closeDanglingFence, type Row, type RowAttachment } from "@/lib/transcript";
import { mediaKindForContentType } from "@/lib/attachments";
import { StatusDot } from "@/components/common/bits";
import { Markdown } from "./Markdown";
import { ThoughtBlock } from "./ThoughtBlock";
import { ActivityCluster } from "./ToolChip";
import { TurnSummary } from "./TurnSummary";
import { FileChangeCards } from "./FileChangeCards";
import { TurnActions } from "./TurnActions";
import { Button, Modal, ModalBody, ModalFooter, ModalHeader } from "@ryuzi/ui";

function MediaItem({ a, onOpenImage }: { a: RowAttachment; onOpenImage: (src: string) => void }) {
  const [failed, setFailed] = useState(false);
  const kind = mediaKindForContentType(a.contentType, a.path);
  const src = convertFileSrc(a.path);
  if (failed || kind === "file") {
    const Icon = kind === "audio" ? AudioLines : Paperclip;
    return (
      <span
        title={a.path}
        className="flex max-w-[220px] items-center gap-1.5 rounded-full border border-border px-2.5 py-1 text-[12px] text-muted-foreground"
      >
        <Icon aria-hidden size={12} strokeWidth={2} className="size-3 shrink-0" />
        <span className="truncate">{a.name}</span>
      </span>
    );
  }
  if (kind === "image") {
    return (
      <button type="button" title={a.name} onClick={() => onOpenImage(src)} className="cursor-zoom-in">
        <img src={src} alt={a.name} onError={() => setFailed(true)} className="max-h-40 rounded-lg border border-border" />
      </button>
    );
  }
  if (kind === "video") {
    return (
      // biome-ignore lint/a11y/useMediaCaption: user-supplied attachment, no caption track exists to offer
      <video controls src={src} onError={() => setFailed(true)} className="max-h-52 w-full max-w-[420px] rounded-lg border border-border" />
    );
  }
  // biome-ignore lint/a11y/useMediaCaption: user-supplied attachment, no caption track exists to offer
  return <audio controls src={src} onError={() => setFailed(true)} className="w-[300px]" />;
}

function UserBubble({
  text,
  attachments,
  onOpenImage,
}: {
  text: string;
  attachments: RowAttachment[];
  onOpenImage: (src: string) => void;
}) {
  return (
    <div className="flex flex-col items-end gap-1.5">
      {attachments.length > 0 && (
        <div className="flex max-w-[70%] flex-wrap justify-end gap-1.5">
          {attachments.map((a) => (
            <MediaItem key={a.path} a={a} onOpenImage={onOpenImage} />
          ))}
        </div>
      )}
      {text.trim() && (
        <div className="max-w-[70%] self-end whitespace-pre-wrap rounded-xl bg-secondary px-3.5 py-2.5 text-[13.5px] leading-[1.55] text-secondary-foreground">
          {text}
        </div>
      )}
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

// System notices (e.g. compaction) render as a muted centered chip — distinct
// from error rows and outside the user/agent bubble flow.
function NoticeRow({ text }: { text: string }) {
  return (
    <div className="my-2 flex justify-center">
      <span className="rounded-full bg-muted px-3 py-0.5 text-[11px] text-muted-foreground">{text}</span>
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
  sessionPk,
  rows,
  agentName,
  agentColor,
  running,
  children,
}: {
  sessionPk: string;
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
  const [lightbox, setLightbox] = useState<string | null>(null);

  const groups = useMemo(() => buildTranscript(rows, running), [rows, running]);
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

  // Keyed on group count AND tail growth: with coalescing, a growing turn
  // changes length (text/thought) or item count (activity clusters), not group count.
  // biome-ignore lint/correctness/useExhaustiveDependencies: scroll pinning reacts to transcript growth, not identity
  useEffect(() => {
    if (stickRef.current) scrollRef.current?.scrollTo({ top: scrollRef.current.scrollHeight });
  }, [groups.length, tailLen, running]);

  return (
    <>
      <div ref={scrollRef} onScroll={onScroll} className="min-h-0 flex-1 overflow-y-auto px-6 py-5">
        <div className="mx-auto flex w-full max-w-3xl flex-col gap-3.5">
          {groups.map((g, i) => {
            const streamingTail = running && i === groups.length - 1;
            switch (g.type) {
              case "user":
                return <UserBubble key={g.key} text={g.text} attachments={g.attachments} onOpenImage={setLightbox} />;
              case "agent":
                return (
                  <div key={g.key} className="flex flex-col">
                    <AgentTurn
                      markdown={streamingTail ? closeDanglingFence(g.markdown) : g.markdown}
                      agentName={agentName}
                      agentColor={agentColor}
                    />
                    {g.turnEnd === true && <TurnActions markdown={g.markdown} />}
                  </div>
                );
              case "thought":
                return <ThoughtBlock key={g.key} markdown={g.markdown} streaming={streamingTail} />;
              case "activity":
                return <ActivityCluster key={g.key} items={g.items} live={running} />;
              case "error":
                return <ErrorRow key={g.key} text={g.text} />;
              case "notice":
                return <NoticeRow key={g.key} text={g.text} />;
              case "summary":
                return (
                  <div key={g.key} className="flex flex-col gap-1.5">
                    <TurnSummary groups={g.groups} durationMs={g.durationMs} />
                    <FileChangeCards sessionPk={sessionPk} cards={g.editCards} />
                  </div>
                );
              default:
                return null;
            }
          })}
          {running && <WorkingPulse color={agentColor} />}
          {children}
        </div>
      </div>
      {lightbox !== null && (
        <Modal onClose={() => setLightbox(null)} width={960}>
          <ModalHeader title="Image preview" />
          <ModalBody className="flex justify-center">
            <img src={lightbox} alt="attachment preview" className="max-h-[70vh] max-w-full rounded-lg" />
          </ModalBody>
          <ModalFooter>
            <Button variant="outline" onClick={() => setLightbox(null)}>
              Close
            </Button>
          </ModalFooter>
        </Modal>
      )}
    </>
  );
}
