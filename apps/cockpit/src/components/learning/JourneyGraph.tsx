import { useMemo, useState } from "react";
import { Pin, PinOff, Sparkles } from "lucide-react";
import { Button } from "@ryuzi/ui";
import type { LearningGraph, LearningGraphNode, SkillUsage } from "@/bindings";
import { useLearning } from "@/store-learning";

// From-scratch SVG journey graph (no graph lib in package.json — see
// components/session/ContextRing.tsx for the pattern this follows: a fixed
// viewBox, deterministic positions computed from content-stable ids, scaled
// with `width="100%"` so the container never reflows as data loads).

const WIDTH = 640;
const HEIGHT = 260;
const MARGIN = 36;
const SKILL_Y = HEIGHT * 0.66;
const MEMORY_BAND_TOP = HEIGHT * 0.14;
const ORPHAN_Y = HEIGHT * 0.94;
const R_MIN = 5;
const R_MAX = 13;

const SKILL_STATE_COLOR: Record<string, string> = {
  active: "#22C55E",
  stale: "#F59E0B",
  archived: "#9CA3AF",
};

const MEMORY_SCOPE_COLOR: Record<string, string> = {
  global: "#3B82F6",
  user: "#A855F7",
  project: "#F97316",
};

type PositionedSkill = { node: LearningGraphNode; x: number; y: number; r: number; useCount: number };
type PositionedMemory = { node: LearningGraphNode; x: number; y: number };
type PositionedEdge = { key: string; kind: string; x1: number; y1: number; x2: number; y2: number };

type Layout = { skills: PositionedSkill[]; memory: PositionedMemory[]; edges: PositionedEdge[] };

/** Deterministic layout: skill nodes ranked left-to-right by `use_count`
 *  (joined from `listSkillUsage` — `LearningGraphNode` itself carries no use
 *  count, per Task-12 resolution #3) and sized by the same signal; memory
 *  nodes cluster above the skill(s) their `lexical` edges point at, or sit in
 *  an orphan row (alphabetical, for stable order) when unconnected. Pure so
 *  it's testable without mounting the SVG. */
export function layoutJourneyGraph(graph: LearningGraph, skillUsage: SkillUsage[]): Layout {
  const useCountByName = new Map(skillUsage.map((s) => [s.name, s.useCount]));
  const skillNodes = graph.nodes
    .filter((n) => n.kind === "skill")
    .slice()
    .sort((a, b) => (useCountByName.get(b.label) ?? 0) - (useCountByName.get(a.label) ?? 0) || a.label.localeCompare(b.label));
  const maxUse = Math.max(1, ...skillNodes.map((n) => useCountByName.get(n.label) ?? 0));

  const positionedSkills: PositionedSkill[] = skillNodes.map((node, i) => {
    const useCount = useCountByName.get(node.label) ?? 0;
    const x = skillNodes.length === 1 ? WIDTH / 2 : MARGIN + (i * (WIDTH - 2 * MARGIN)) / (skillNodes.length - 1);
    const r = R_MIN + (R_MAX - R_MIN) * (useCount / maxUse);
    return { node, x, y: SKILL_Y, r, useCount };
  });
  const skillPosById = new Map(positionedSkills.map((p) => [p.node.id, p]));

  const memoryNodes = graph.nodes.filter((n) => n.kind === "memory");
  const linkedSkillIdsByMemory = new Map<string, string[]>();
  for (const e of graph.edges) {
    if (e.kind !== "lexical") continue;
    const memId = skillPosById.has(e.source) ? e.target : e.source;
    const skillId = skillPosById.has(e.source) ? e.source : e.target;
    if (!skillPosById.has(skillId)) continue;
    const list = linkedSkillIdsByMemory.get(memId) ?? [];
    list.push(skillId);
    linkedSkillIdsByMemory.set(memId, list);
  }

  // Bucket linked memory nodes by their anchor skill so siblings stack
  // vertically instead of overlapping at the same x.
  const stackIndexByAnchor = new Map<string, number>();
  const positionedMemory: PositionedMemory[] = [];
  const orphans: LearningGraphNode[] = [];
  for (const node of memoryNodes) {
    const linkedIds = linkedSkillIdsByMemory.get(node.id);
    if (!linkedIds || linkedIds.length === 0) {
      orphans.push(node);
      continue;
    }
    const xs = linkedIds.map((id) => skillPosById.get(id)?.x ?? WIDTH / 2);
    const anchorX = xs.reduce((a, b) => a + b, 0) / xs.length;
    const anchorKey = String(Math.round(anchorX / 24));
    const stack = stackIndexByAnchor.get(anchorKey) ?? 0;
    stackIndexByAnchor.set(anchorKey, stack + 1);
    positionedMemory.push({ node, x: anchorX, y: Math.max(16, MEMORY_BAND_TOP - stack * 20) });
  }
  orphans.sort((a, b) => (a.scope ?? "").localeCompare(b.scope ?? "") || a.label.localeCompare(b.label));
  orphans.forEach((node, i) => {
    const x = orphans.length === 1 ? WIDTH / 2 : MARGIN + (i * (WIDTH - 2 * MARGIN)) / (orphans.length - 1);
    positionedMemory.push({ node, x, y: ORPHAN_Y });
  });
  const memoryPosById = new Map(positionedMemory.map((p) => [p.node.id, p]));

  const edges: PositionedEdge[] = [];
  for (const e of graph.edges) {
    const a = skillPosById.get(e.source) ?? memoryPosById.get(e.source);
    const b = skillPosById.get(e.target) ?? memoryPosById.get(e.target);
    if (!a || !b) continue;
    edges.push({ key: `${e.source}->${e.target}:${e.kind}`, kind: e.kind, x1: a.x, y1: a.y, x2: b.x, y2: b.y });
  }

  return { skills: positionedSkills, memory: positionedMemory, edges };
}

export function JourneyGraph({ graph, skillUsage }: { graph: LearningGraph; skillUsage: SkillUsage[] }) {
  const [selected, setSelected] = useState<string | null>(null);
  const layout = useMemo(() => layoutJourneyGraph(graph, skillUsage), [graph, skillUsage]);
  const selectedSkill = useMemo(() => (selected ? skillUsage.find((s) => s.name === selected) : undefined), [selected, skillUsage]);
  const empty = layout.skills.length === 0 && layout.memory.length === 0;

  return (
    <div className="flex flex-col gap-2">
      <div className="relative w-full overflow-hidden rounded-lg border border-border bg-muted/20" style={{ height: HEIGHT }}>
        {empty ? (
          <div className="flex h-full items-center justify-center px-6 text-center text-[12.5px] text-muted-foreground">
            No skills or memory yet — the learning loop populates this graph as sessions run.
          </div>
        ) : (
          <svg width="100%" height="100%" viewBox={`0 0 ${WIDTH} ${HEIGHT}`} role="img" aria-label="Learning journey graph">
            {layout.edges.map((e) => (
              <line
                key={e.key}
                x1={e.x1}
                y1={e.y1}
                x2={e.x2}
                y2={e.y2}
                stroke="currentColor"
                className="text-border"
                strokeWidth={e.kind === "related_skills" ? 1.25 : 1}
                strokeDasharray={e.kind === "lexical" ? "3 3" : undefined}
              />
            ))}
            {layout.memory.map((m) => {
              const color = MEMORY_SCOPE_COLOR[m.node.scope ?? ""] ?? "#9CA3AF";
              const s = 6;
              return (
                <g key={m.node.id}>
                  <rect
                    x={m.x - s / 2}
                    y={m.y - s / 2}
                    width={s}
                    height={s}
                    fill={color}
                    opacity={0.85}
                    transform={`rotate(45 ${m.x} ${m.y})`}
                  />
                  <title>{m.node.label}</title>
                </g>
              );
            })}
            {layout.skills.map((p) => {
              const color = SKILL_STATE_COLOR[p.node.state ?? ""] ?? "#9CA3AF";
              const isSelected = selected === p.node.label;
              return (
                // biome-ignore lint/a11y/useSemanticElements: an SVG <g> of <circle>/<text> primitives can't be a <button> — role+key handling below covers the same accessibility contract.
                <g
                  key={p.node.id}
                  role="button"
                  tabIndex={0}
                  aria-pressed={isSelected}
                  aria-label={`${p.node.label} — ${p.node.state ?? "unknown"} — used ${p.useCount} times`}
                  onClick={() => setSelected(isSelected ? null : p.node.label)}
                  onKeyDown={(e) => {
                    if (e.key !== "Enter" && e.key !== " ") return;
                    e.preventDefault();
                    setSelected(isSelected ? null : p.node.label);
                  }}
                  className="cursor-pointer outline-none"
                >
                  <circle
                    cx={p.x}
                    cy={p.y}
                    r={p.r}
                    fill={color}
                    stroke={isSelected ? "currentColor" : "none"}
                    className={isSelected ? "text-foreground" : undefined}
                    strokeWidth={isSelected ? 2 : 0}
                  />
                  <text x={p.x} y={p.y + p.r + 12} textAnchor="middle" className="fill-muted-foreground" style={{ fontSize: 9.5 }}>
                    {p.node.label.length > 14 ? `${p.node.label.slice(0, 13)}…` : p.node.label}
                  </text>
                  <title>
                    {p.node.label} — {p.node.state ?? "unknown"} — used {p.useCount}×
                  </title>
                </g>
              );
            })}
          </svg>
        )}
      </div>

      <div className="flex flex-wrap items-center gap-x-4 gap-y-1 text-[11px] text-muted-foreground">
        <span className="flex items-center gap-1.5">
          <Sparkles aria-hidden size={11} strokeWidth={2} />
          Skills ranked by use — dot size and left-to-right order both track use_count.
        </span>
        <span className="flex items-center gap-1">
          <span aria-hidden className="inline-block size-2 rounded-full" style={{ background: SKILL_STATE_COLOR.active }} />
          Active
        </span>
        <span className="flex items-center gap-1">
          <span aria-hidden className="inline-block size-2 rounded-full" style={{ background: SKILL_STATE_COLOR.stale }} />
          Stale
        </span>
        <span className="flex items-center gap-1">
          <span aria-hidden className="inline-block size-2 rounded-full" style={{ background: SKILL_STATE_COLOR.archived }} />
          Archived
        </span>
        <span className="flex items-center gap-1">
          <span aria-hidden className="inline-block size-1.5 rotate-45" style={{ background: MEMORY_SCOPE_COLOR.global }} />
          Memory (diamond, color = scope)
        </span>
      </div>

      {selectedSkill && (
        <div className="flex items-center gap-2.5 rounded-lg border border-border bg-muted/30 px-3 py-2">
          <span className="min-w-0 flex-1 text-[12.5px]">
            <span className="font-medium">{selectedSkill.name}</span>{" "}
            <span className="text-muted-foreground">
              · {selectedSkill.state} · used {selectedSkill.useCount}× · {selectedSkill.patchCount} patch
              {selectedSkill.patchCount === 1 ? "" : "es"}
            </span>
          </span>
          <PinToggle skill={selectedSkill} />
        </div>
      )}
    </div>
  );
}

function PinToggle({ skill }: { skill: SkillUsage }) {
  const setSkillPinned = useLearning((s) => s.setSkillPinned);
  return (
    <Button
      type="button"
      variant="ghost"
      size="icon-xs"
      title={skill.pinned ? "Unpin" : "Pin"}
      onClick={() => void setSkillPinned(skill.name, !skill.pinned)}
    >
      {skill.pinned ? <Pin aria-hidden size={13} strokeWidth={2} fill="currentColor" /> : <PinOff aria-hidden size={13} strokeWidth={2} />}
    </Button>
  );
}
