import { useContext, useMemo, useState, type ReactNode } from "react";
import { Button } from "@ryuzi/ui";
import { toJsxRuntime } from "hast-util-to-jsx-runtime";
import { common, createLowlight } from "lowlight";
import { Fragment, jsx, jsxs } from "react/jsx-runtime";
import {
  Brain,
  Check,
  ChevronRight,
  Copy,
  FileText,
  FolderInput,
  Globe,
  Loader2,
  Pencil,
  RefreshCw,
  Search,
  SquareTerminal,
  Trash2,
  Wrench,
  X,
  type LucideIcon,
} from "lucide-react";
import { toWorkspaceRelativePath } from "@/lib/paths";
import { formatToolDuration, partitionActivity, toolCardHeader, type ActivityFragment, type ActivityItem } from "@/lib/transcript";
import { TranscriptFileContext, useOpenWorkspaceFile } from "./TranscriptFileContext";

const kindIcon: Record<string, LucideIcon> = {
  read: FileText,
  edit: Pencil,
  delete: Trash2,
  move: FolderInput,
  search: Search,
  execute: SquareTerminal,
  think: Brain,
  fetch: Globe,
  switch_mode: RefreshCw,
};

const lowlight = createLowlight(common);
/** Auto-detection above this size is too slow to be worth it. */
const HIGHLIGHT_MAX_CHARS = 10_000;
/** Below this lowlight relevance the language guess is noise — render plain. */
const HIGHLIGHT_MIN_RELEVANCE = 5;

function StatusMark({ status }: { status: string | null }) {
  if (status === "completed") return <Check aria-hidden size={12} strokeWidth={2.5} style={{ color: "#22C55E" }} />;
  if (status === "failed") return <X aria-hidden size={12} strokeWidth={2.5} className="text-destructive" />;
  // pending | in_progress | unknown → still running
  return <Loader2 aria-hidden size={12} strokeWidth={2} className="animate-spin text-muted-foreground" />;
}

/** Monospace output body. The `chat-md` wrapper reuses the app-token hljs
 *  theme from index.css; lowlight highlights only when its auto-detection is
 *  confident, otherwise the text renders plain. */
function OutputBlock({ text }: { text: string }) {
  const [copied, setCopied] = useState(false);
  const highlighted = useMemo(() => {
    if (text.length > HIGHLIGHT_MAX_CHARS) return null;
    try {
      const tree = lowlight.highlightAuto(text);
      const relevance = (tree.data as { relevance?: number } | undefined)?.relevance ?? 0;
      return relevance >= HIGHLIGHT_MIN_RELEVANCE ? toJsxRuntime(tree, { Fragment, jsx, jsxs }) : null;
    } catch {
      return null;
    }
  }, [text]);
  const copy = () => {
    void navigator.clipboard.writeText(text).then(() => {
      setCopied(true);
      setTimeout(() => setCopied(false), 1500);
    });
  };
  return (
    <div className="chat-md relative border-t border-border">
      <Button
        variant="ghost"
        size="icon-xs"
        title="Copy output"
        onClick={copy}
        className="absolute right-1.5 top-1.5 z-10 text-muted-foreground"
      >
        {copied ? <Check aria-hidden size={12} strokeWidth={2} /> : <Copy aria-hidden size={12} strokeWidth={2} />}
      </Button>
      {/* inline style beats the `.chat-md pre` chrome (margin/border/radius)
          so the block sits flush inside the card */}
      <pre className="max-h-72 overflow-auto" style={{ margin: 0, border: "none", borderRadius: 0 }}>
        <code className="hljs">{highlighted ?? text}</code>
      </pre>
    </div>
  );
}

function Badge({ children, tone = "muted" }: { children: ReactNode; tone?: "muted" | "error" }) {
  return (
    <span
      className={
        tone === "error"
          ? "shrink-0 rounded border border-destructive/40 bg-destructive/10 px-1.5 py-px text-[10.5px] text-destructive"
          : "shrink-0 rounded border border-border px-1.5 py-px text-[10.5px] text-muted-foreground"
      }
    >
      {children}
    </span>
  );
}

function ToolChip({ item, live }: { item: Extract<ActivityItem, { type: "tool" }>; live: boolean }) {
  // Live turns show their work as it happens; completed turns (rendered inside
  // TurnSummary) mount with live=false and start collapsed.
  const [open, setOpen] = useState(live);
  const Icon = (item.kind && kindIcon[item.kind]) || Wrench;
  const { title, detail } = toolCardHeader(item);
  const duration = formatToolDuration(item.durationMs);
  const expandable = !!item.output;
  const openWorkspaceFile = useOpenWorkspaceFile();
  const fileCtx = useContext(TranscriptFileContext);
  // Trusted arg path (no probe): clickable when a provider is present and the
  // path resolves inside the worktree.
  const linkRel = openWorkspaceFile && fileCtx && item.path ? toWorkspaceRelativePath(item.path, fileCtx.workdir) : null;

  const iconTitle = (
    <>
      <Icon aria-hidden size={12} strokeWidth={2} className="shrink-0 text-muted-foreground" />
      <span className="shrink-0">{title}</span>
    </>
  );
  const trailing = (
    <>
      {duration !== "" && <Badge>{duration}</Badge>}
      {item.exitCode !== null && <Badge tone={item.exitCode === 0 ? "muted" : "error"}>exit {item.exitCode}</Badge>}
      <StatusMark status={item.status} />
    </>
  );
  const headerClass = "flex w-full items-center gap-2 px-3 py-[7px] font-mono text-xs text-foreground";

  let headerNode: ReactNode;
  if (linkRel !== null && openWorkspaceFile !== null) {
    // A clickable path is present, so the row renders a real <a>. An anchor
    // nested inside a <button> is invalid content (and, unlike
    // button-in-button, browsers won't auto-repair it), so the toggle target
    // (icon+title only) and the link must be siblings inside a plain div.
    const detailNode =
      detail !== null ? (
        <a
          href={linkRel}
          className="min-w-0 flex-1 cursor-pointer truncate text-left font-normal text-muted-foreground underline decoration-dotted underline-offset-2"
          onClick={(e) => {
            e.preventDefault();
            e.stopPropagation();
            openWorkspaceFile(linkRel);
          }}
          onAuxClick={(e) => e.preventDefault()}
          draggable={false}
        >
          {detail}
        </a>
      ) : (
        <span className="min-w-0 flex-1" />
      );
    headerNode = (
      <div className={headerClass}>
        {expandable ? (
          <Button
            variant="ghost"
            size="xs"
            aria-expanded={open}
            onClick={() => setOpen((v) => !v)}
            className="h-auto shrink-0 cursor-pointer gap-2 rounded p-0.5 font-normal hover:bg-accent dark:hover:bg-accent"
          >
            {iconTitle}
          </Button>
        ) : (
          iconTitle
        )}
        {detailNode}
        {trailing}
      </div>
    );
  } else {
    // No clickable path on this card: the whole header row is the toggle
    // again (original pre-4c34f40 behavior) — full-row hover affordance and
    // click target, with detail rendered as a plain, non-interactive span.
    const header = (
      <>
        {iconTitle}
        {detail !== null ? (
          <span className="min-w-0 flex-1 truncate text-left font-normal text-muted-foreground">{detail}</span>
        ) : (
          <span className="min-w-0 flex-1" />
        )}
        {trailing}
      </>
    );
    headerNode = expandable ? (
      <Button
        variant="ghost"
        size="xs"
        aria-expanded={open}
        onClick={() => setOpen((v) => !v)}
        className={`${headerClass} h-auto cursor-pointer justify-start rounded-none font-normal hover:bg-accent dark:hover:bg-accent`}
      >
        {header}
      </Button>
    ) : (
      <div className={headerClass}>{header}</div>
    );
  }

  return (
    <div className="acrylic-panel flex w-full max-w-[640px] flex-col overflow-hidden rounded-md border border-border">
      {headerNode}
      {open && item.output && <OutputBlock text={item.output} />}
    </div>
  );
}

function StatusChip({ text }: { text: string }) {
  return (
    <div className="acrylic-panel flex max-w-fit items-center gap-2 rounded-md border border-border px-3 py-[7px] font-mono text-xs text-muted-foreground">
      <span aria-hidden style={{ color: "#22C55E" }}>
        ›
      </span>
      <span className="text-foreground">{text}</span>
    </div>
  );
}

/** A folded run of steps: one muted "See N steps" row, expanding to the
 *  individual chips (which mount collapsed, like a completed turn's). */
function StepsFold({ items, runLength }: { items: ActivityItem[]; runLength: number }) {
  const [open, setOpen] = useState(false);
  const label = `See ${runLength} step${runLength === 1 ? "" : "s"}`;
  return (
    <div className="flex flex-col gap-1.5">
      <Button
        variant="ghost"
        size="xs"
        aria-expanded={open}
        onClick={() => setOpen((v) => !v)}
        className="h-auto max-w-fit cursor-pointer justify-start gap-1.5 px-1.5 py-[3px] font-mono text-xs font-normal text-muted-foreground"
      >
        <ChevronRight aria-hidden size={12} strokeWidth={2} className={open ? "rotate-90 transition-transform" : "transition-transform"} />
        {label}
      </Button>
      {open &&
        items.map((item) =>
          item.type === "tool" ? <ToolChip key={item.key} item={item} live={false} /> : <StatusChip key={item.key} text={item.text} />,
        )}
    </div>
  );
}

/** A run of consecutive tool calls / status rows, stacked compactly. `live`
 *  is true only for the streaming turn — its cards mount expanded. `fold`
 *  partitions the run behind "See N steps" rows (live turns only today);
 *  `liveTail` keeps the newest STREAMING_TAIL items visible while folding
 *  older ones. */
export function ActivityCluster({
  items,
  live = false,
  fold = false,
  liveTail = false,
}: {
  items: ActivityItem[];
  live?: boolean;
  fold?: boolean;
  liveTail?: boolean;
}) {
  if (!fold) {
    return (
      <div className="flex flex-col gap-1.5">
        {items.map((item) =>
          item.type === "tool" ? <ToolChip key={item.key} item={item} live={live} /> : <StatusChip key={item.key} text={item.text} />,
        )}
      </div>
    );
  }
  const fragments: ActivityFragment[] = partitionActivity(items, liveTail);
  return (
    <div className="flex flex-col gap-1.5">
      {fragments.map((f) =>
        f.kind === "fold" ? (
          <StepsFold key={f.items[0].key} items={f.items} runLength={f.runLength} />
        ) : f.item.type === "tool" ? (
          <ToolChip key={f.item.key} item={f.item} live={live} />
        ) : (
          <StatusChip key={f.item.key} text={f.item.text} />
        ),
      )}
    </div>
  );
}
