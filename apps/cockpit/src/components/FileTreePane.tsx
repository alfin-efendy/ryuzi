import { ChevronRight, FileText } from "lucide-react";
import { useCallback, useEffect, useState } from "react";
import { commands } from "@/bindings";
import { useUi } from "@/store-ui";

type Node = { rel: string; name: string; dir: boolean; depth: number; open?: boolean; children?: Node[] };

// Real lazy file tree over the session worktree; clicking a file opens it in
// the dock's file viewer through the existing read_file path.
export function FileTreePane({ sessionPk, filter }: { sessionPk: string; filter: string }) {
  const openFile = useUi((s) => s.openFile);
  const [root, setRoot] = useState<Node[]>([]);
  const [workdir, setWorkdir] = useState<string | null>(null);

  const load = useCallback(
    async (rel: string, depth: number): Promise<Node[]> => {
      const res = await commands.listDir(sessionPk, rel);
      if (res.status !== "ok") return [];
      return res.data.map((e) => ({
        rel: rel ? `${rel}/${e.name}` : e.name,
        name: e.name,
        dir: e.dir,
        depth,
      }));
    },
    [sessionPk],
  );

  useEffect(() => {
    void load("", 0).then(setRoot);
    void commands.sessionWorkdir(sessionPk).then((res) => {
      if (res.status === "ok") setWorkdir(res.data);
    });
  }, [sessionPk, load]);

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
    const sep = workdir.includes("\\") ? "\\" : "/";
    openFile(`${workdir}${sep}${node.rel.split("/").join(sep)}`);
  };

  const flatten = (nodes: Node[]): Node[] =>
    nodes.flatMap((n) => (n.open && n.children ? [n, ...flatten(n.children)] : [n]));

  const needle = filter.trim().toLowerCase();
  const visible = flatten(root).filter((n) => needle === "" || n.rel.toLowerCase().includes(needle));

  return (
    <div className="flex flex-col">
      {visible.length === 0 && <div className="px-2 py-2 text-[12px] text-muted-foreground">No files.</div>}
      {visible.map((n) => (
        <button
          key={n.rel}
          type="button"
          onClick={() => (n.dir ? void toggleDir(n) : openLeaf(n))}
          className="flex cursor-pointer items-center gap-1.5 rounded-sm border-none bg-transparent py-[5px] pr-2 text-left font-sans text-[12.5px] text-foreground hover:bg-accent"
          style={{ paddingLeft: 8 + n.depth * 14 }}
        >
          {n.dir ? (
            <ChevronRight
              aria-hidden
              size={11}
              strokeWidth={2}
              className="shrink-0 text-muted-foreground transition-transform duration-100"
              style={{ transform: n.open ? "rotate(90deg)" : undefined }}
            />
          ) : (
            <FileText aria-hidden size={12} strokeWidth={2} className="shrink-0 text-muted-foreground" />
          )}
          <span className="truncate">{n.name}</span>
        </button>
      ))}
    </div>
  );
}
