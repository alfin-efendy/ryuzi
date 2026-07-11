import { memo, useEffect, useMemo, useRef, useState, type ReactNode } from "react";
import { AudioLines, ChevronDown, Paperclip } from "lucide-react";
import { commands } from "@/bindings";
import { buildTranscript, closeDanglingFence, type Row, type RowAttachment } from "@/lib/transcript";
import { mediaKindForContentType } from "@/lib/attachments";
import { distanceFromBottom, isStuck, pinningInterrupted, showScrollFab } from "@/lib/scroll";
import { StatusDot } from "@/components/common/bits";
import { Markdown } from "./Markdown";
import { ThoughtBlock } from "./ThoughtBlock";
import { ActivityCluster } from "./ToolChip";
import { TurnSummary } from "./TurnSummary";
import { FileChangeCards } from "./FileChangeCards";
import { TurnActions } from "./TurnActions";

/** Renders one saved attachment. Unlike the pre-P4-3 `convertFileSrc(a.path)`
 *  (Tauri's asset protocol, which only ever reads THIS host's disk), the
 *  bytes are fetched through the engine's authed, jailed `GET /attachments`
 *  route (`commands.fetchAttachment`) — remote-safe, since a remote runner's
 *  attachments live on ITS disk, not the cockpit's. Non-previewable kinds
 *  (`"file"`) never fetch at all — there's nothing to render beyond the
 *  chip, so the IPC round-trip and base64 blow-up are skipped entirely. */
function MediaItem({ runnerId, a, onOpenImage }: { runnerId: string; a: RowAttachment; onOpenImage: (src: string) => void }) {
  const kind = mediaKindForContentType(a.contentType, a.path);
  const [src, setSrc] = useState<string | null>(null);
  const [failed, setFailed] = useState(false);

  useEffect(() => {
    let cancelled = false;
    setSrc(null);
    setFailed(false);
    if (kind === "file") return;
    void commands.fetchAttachment(runnerId, a.rel).then((res) => {
      if (cancelled) return;
      if (res.status === "ok") {
        const mime = a.contentType ?? res.data.contentType ?? "application/octet-stream";
        setSrc(`data:${mime};base64,${res.data.dataBase64}`);
      } else {
        setFailed(true);
      }
    });
    return () => {
      cancelled = true;
    };
  }, [runnerId, a.rel, a.contentType, kind]);

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
  if (!src) {
    // Loading: same rounded footprint as the eventual media so the bubble
    // doesn't jump once the fetch resolves.
    return (
      <span className="flex h-10 w-16 animate-pulse items-center justify-center rounded-lg border border-border bg-muted/40" aria-hidden />
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
  runnerId,
  text,
  attachments,
  onOpenImage,
}: {
  runnerId: string;
  text: string;
  attachments: RowAttachment[];
  onOpenImage: (src: string) => void;
}) {
  return (
    <div className="flex flex-col items-end gap-1.5">
      {attachments.length > 0 && (
        <div className="flex max-w-[70%] flex-wrap justify-end gap-1.5">
          {attachments.map((a) => (
            <MediaItem key={a.rel || a.path} runnerId={runnerId} a={a} onOpenImage={onOpenImage} />
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
  runnerId,
  sessionPk,
  rows,
  agentName,
  agentColor,
  running,
  children,
}: {
  runnerId: string;
  sessionPk: string;
  rows: Row[];
  agentName: string;
  agentColor: string;
  running: boolean;
  children?: ReactNode;
}) {
  const scrollRef = useRef<HTMLDivElement>(null);
  const contentRef = useRef<HTMLDivElement>(null);
  // Stick to the bottom only while the user is already there; scrolling up
  // to read pauses the auto-scroll until they return to the bottom.
  const stickRef = useRef(true);
  // True while a FAB-initiated smooth scroll is in flight, so its own scroll
  // events don't read as "the user scrolled away" and cancel the stick.
  const pinningRef = useRef(false);
  // Distance-from-bottom observed at the previous scroll event, used to
  // detect the user interrupting a pinned flight (distance growing instead
  // of shrinking) so control can be handed back to them mid-scroll.
  const lastDistRef = useRef(0);
  const [fabVisible, setFabVisible] = useState(false);
  const [lightbox, setLightbox] = useState<string | null>(null);

  const groups = useMemo(() => buildTranscript(rows, running), [rows, running]);

  const onScroll = () => {
    const el = scrollRef.current;
    if (!el) return;
    const d = distanceFromBottom(el.scrollHeight, el.scrollTop, el.clientHeight);
    if (pinningRef.current) {
      if (isStuck(d)) {
        pinningRef.current = false;
      } else if (pinningInterrupted(lastDistRef.current, d)) {
        pinningRef.current = false;
        stickRef.current = false;
      }
    } else {
      stickRef.current = isStuck(d);
    }
    lastDistRef.current = d;
    setFabVisible(!pinningRef.current && showScrollFab(d));
  };

  const scrollToBottom = () => {
    const el = scrollRef.current;
    if (!el) return;
    stickRef.current = true;
    pinningRef.current = true;
    lastDistRef.current = distanceFromBottom(el.scrollHeight, el.scrollTop, el.clientHeight);
    setFabVisible(false);
    const reduced = window.matchMedia?.("(prefers-reduced-motion: reduce)").matches;
    el.scrollTo({ top: el.scrollHeight, behavior: reduced ? "auto" : "smooth" });
  };

  // Pin via ResizeObserver: ANY content growth (streaming text, images and
  // markdown settling their height, new groups) re-pins while stuck — the
  // old effect keyed on group count/tail length missed post-render growth.
  useEffect(() => {
    const el = scrollRef.current;
    const content = contentRef.current;
    if (!el || !content || typeof ResizeObserver === "undefined") return;
    const ro = new ResizeObserver(() => {
      if (stickRef.current) el.scrollTop = el.scrollHeight;
    });
    ro.observe(content);
    return () => ro.disconnect();
  }, []);

  // Opening or switching sessions always lands at the latest message.
  // biome-ignore lint/correctness/useExhaustiveDependencies: re-runs on session identity change, not on the refs it touches
  useEffect(() => {
    stickRef.current = true;
    lastDistRef.current = 0;
    setFabVisible(false);
    const el = scrollRef.current;
    if (el) el.scrollTop = el.scrollHeight;
  }, [sessionPk]);

  return (
    <>
      <div className="relative flex min-h-0 flex-1 flex-col">
        <div ref={scrollRef} onScroll={onScroll} className="min-h-0 flex-1 overflow-y-auto px-6 py-5">
          <div ref={contentRef} className="mx-auto flex w-full max-w-3xl flex-col gap-3.5">
            {groups.map((g, i) => {
              const streamingTail = running && i === groups.length - 1;
              switch (g.type) {
                case "user":
                  return <UserBubble key={g.key} runnerId={runnerId} text={g.text} attachments={g.attachments} onOpenImage={setLightbox} />;
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
                  return <ActivityCluster key={g.key} items={g.items} live={running} fold={running} liveTail={streamingTail} />;
                case "error":
                  return <ErrorRow key={g.key} text={g.text} />;
                case "notice":
                  return <NoticeRow key={g.key} text={g.text} />;
                case "summary":
                  return (
                    <div key={g.key} className="flex flex-col gap-1.5">
                      <TurnSummary groups={g.groups} durationMs={g.durationMs} />
                      <FileChangeCards runnerId={runnerId} sessionPk={sessionPk} cards={g.editCards} />
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
        {fabVisible && (
          <button
            type="button"
            aria-label="Scroll to bottom"
            onClick={scrollToBottom}
            className="acrylic-card absolute bottom-4 right-6 z-20 flex size-8 items-center justify-center rounded-full border border-border text-muted-foreground shadow-lg transition-opacity hover:text-foreground"
          >
            <ChevronDown aria-hidden size={15} strokeWidth={2} className="size-[15px]" />
          </button>
        )}
      </div>
      {lightbox !== null && (
        <div
          role="dialog"
          aria-label="Image preview"
          tabIndex={-1}
          ref={(el) => el?.focus()}
          className="fixed inset-0 z-50 flex cursor-zoom-out items-center justify-center bg-black/70"
          onClick={() => setLightbox(null)}
          onKeyDown={(e) => e.key === "Escape" && setLightbox(null)}
        >
          <img src={lightbox} alt="attachment preview" className="max-h-[90vh] max-w-[90vw] rounded-lg" />
        </div>
      )}
    </>
  );
}
