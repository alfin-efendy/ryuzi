import { ChevronRight, FileText } from "lucide-react";
import { useCallback, useEffect, useState } from "react";
import { commands } from "@/bindings";
import { joinPath } from "@/lib/paths";
import { useUi } from "@/store-ui";
import { Button } from "@ryuzi/ui";

export type Node = { rel: string; name: string; dir: boolean; depth: number; open?: boolean; children?: Node[] };

/** Rel paths of every expanded directory, for restoring after a reload. */
export function collectOpenDirs(nodes: Node[]): string[] {
  return nodes.flatMap((n) => (n.dir && n.open ? [n.rel, ...(n.children ? collectOpenDirs(n.children) : [])] : []));
}

// Real lazy file tree over the session worktree; clicking a file opens it in
// the dock's file viewer, which reads it through the jailed fsview
// read_file RPC.
export function FileTreePane({
  runnerId,
  sessionPk,
  filter,
  refreshKey,
}: {
  runnerId: string;
  sessionPk: string;
  filter: string;
  refreshKey: number;
}) {
  const openFile = useUi((s) => s.openFile);
  const [root, setRoot] = useState<Node[]>([]);
  const [workdir, setWorkdir] = useState<string | null>(null);

  const load = useCallback(
    async (rel: string, depth: number): Promise<Node[]> => {
      const res = await commands.listDir(runnerId, sessionPk, rel);
      if (res.status !== "ok") return [];
      return res.data.map((e) => ({
        rel: rel ? `${rel}/${e.name}` : e.name,
        name: e.name,
        dir: e.dir,
        depth,
      }));
    },
    [runnerId, sessionPk],
  );

  useEffect(() => {
    void load("", 0).then(setRoot);
    void commands.sessionWorkdir(runnerId, sessionPk).then((res) => {
      if (res.status === "ok") setWorkdir(res.data);
    });
  }, [runnerId, sessionPk, load]);

  const reload = useCallback(async () => {
    const open = new Set(collectOpenDirs(root));
    const rebuild = async (rel: string, depth: number): Promise<Node[]> => {
      const nodes = await load(rel, depth);
      return Promise.all(
        nodes.map(async (n) => (n.dir && open.has(n.rel) ? { ...n, open: true, children: await rebuild(n.rel, depth + 1) } : n)),
      );
    };
    setRoot(await rebuild("", 0));
  }, [root, load]);

  // biome-ignore lint/correctness/useExhaustiveDependencies: refresh is edge-triggered off refreshKey only
  useEffect(() => {
    if (refreshKey > 0) void reload();
  }, [refreshKey]);

  const toggleDir = async (node: Node) => {
    const update = (nodes: Node[]): Node[] =>
      nodes.map((n) => {
        if (n.rel === node.rel) return { ...n, open: !n.open };
        if (n.children) return { ...n, children: update(n.children) };
        return n;
      });
    if (!node.open && !node.children) {
      const children = await load(node.rel, node.depth + 1);
      const attach = (nodes: Node[]): Node[] =>
        nodes.map((n) => {
          if (n.rel === node.rel) return { ...n, children, open: true };
          if (n.children) return { ...n, children: attach(n.children) };
          return n;
        });
      setRoot((r) => attach(r));
      return;
    }
    setRoot((r) => update(r));
  };

  const openLeaf = (node: Node) => {
    if (!workdir) return;
    openFile(joinPath(workdir, node.rel));
  };

  const flatten = (nodes: Node[]): Node[] => nodes.flatMap((n) => (n.open && n.children ? [n, ...flatten(n.children)] : [n]));

  const needle = filter.trim().toLowerCase();
  const visible = flatten(root).filter((n) => needle === "" || n.rel.toLowerCase().includes(needle));

  return (
    <div className="flex flex-col">
      {visible.length === 0 && <div className="px-2 py-2 text-[12px] text-muted-foreground">No files.</div>}
      {visible.map((n) => (
        <Button
          key={n.rel}
          variant="ghost"
          onClick={() => (n.dir ? void toggleDir(n) : openLeaf(n))}
          className="h-auto w-full justify-start gap-1.5 rounded-sm py-[5px] pr-2 text-left"
          style={{ paddingLeft: 8 + n.depth * 14 }}
        >
          {n.dir ? (
            <ChevronRight
              aria-hidden
              size={11}
              strokeWidth={2}
              className="size-[11px] shrink-0 text-muted-foreground transition-transform duration-100"
              style={{ transform: n.open ? "rotate(90deg)" : undefined }}
            />
          ) : (
            <FileText aria-hidden size={12} strokeWidth={2} className="size-3 shrink-0 text-muted-foreground" />
          )}
          <span className="truncate">{n.name}</span>
        </Button>
      ))}
    </div>
  );
}
