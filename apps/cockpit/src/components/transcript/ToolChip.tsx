import { useState } from "react";
import {
  Brain,
  Check,
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
import type { ActivityItem } from "@/lib/transcript";

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

function StatusMark({ status }: { status: string | null }) {
  if (status === "completed") return <Check aria-hidden size={12} strokeWidth={2.5} style={{ color: "#22C55E" }} />;
  if (status === "failed") return <X aria-hidden size={12} strokeWidth={2.5} className="text-destructive" />;
  // pending | in_progress | unknown → still running
  return <Loader2 aria-hidden size={12} strokeWidth={2} className="animate-spin text-muted-foreground" />;
}

function ToolChip({ item }: { item: Extract<ActivityItem, { type: "tool" }> }) {
  const [open, setOpen] = useState(false);
  const Icon = (item.kind && kindIcon[item.kind]) || Wrench;
  const expandable = !!item.output;
  return (
    <div className="flex max-w-fit flex-col">
      <button
        type="button"
        onClick={() => expandable && setOpen((v) => !v)}
        className={`acrylic-panel flex items-center gap-2 rounded-md border border-border px-3 py-[7px] font-mono text-xs text-foreground ${
          expandable ? "cursor-pointer hover:bg-accent" : "cursor-default"
        }`}
      >
        <Icon aria-hidden size={12} strokeWidth={2} className="text-muted-foreground" />
        {item.name}
        <StatusMark status={item.status} />
      </button>
      {open && item.output && (
        <pre className="mt-1 max-h-48 max-w-[560px] overflow-auto rounded-md border border-border bg-code p-2.5 font-mono text-[11.5px] leading-[1.6] text-code-foreground">
          {item.output}
        </pre>
      )}
    </div>
  );
}

function StatusChip({ text }: { text: string }) {
  return (
    <div className="acrylic-panel flex max-w-fit items-center gap-2 rounded-md border border-border px-3 py-[7px] font-mono text-xs text-muted-foreground">
      <span style={{ color: "#22C55E" }}>›</span>
      <span className="text-foreground">{text}</span>
    </div>
  );
}

/** A run of consecutive tool calls / status rows, stacked compactly. */
export function ActivityCluster({ items }: { items: ActivityItem[] }) {
  return (
    <div className="flex flex-col gap-1.5">
      {items.map((item) =>
        item.type === "tool" ? <ToolChip key={item.key} item={item} /> : <StatusChip key={item.key} text={item.text} />,
      )}
    </div>
  );
}
