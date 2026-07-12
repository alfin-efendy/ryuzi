import { useEffect, useState } from "react";
import { ChevronDown, Pencil, Sparkles, Trash2 } from "lucide-react";
import { Button, Combobox, SettingsCard, SettingsCardHeader, SettingsCardTitle, Textarea } from "@ryuzi/ui";
import { MEMORY_SCOPES, useLearning, type MemoryScope } from "@/store-learning";
import { JourneyGraph } from "@/components/learning/JourneyGraph";
import { ReviewFeed } from "@/components/learning/ReviewFeed";
import { CuratorCard } from "@/components/learning/CuratorCard";

function scopeLabel(scope: MemoryScope): string {
  switch (scope) {
    case "global":
      return "Global";
    case "user":
      return "User";
    case "project":
      return "Project";
  }
}

/** Memory editor: add/edit/remove durable-fact entries per scope through
 *  `write_memory`'s add/replace/remove actions. An entry's own current text
 *  is its `match` when editing/removing it — `write_memory` requires the
 *  matcher to identify exactly one entry (crates/core/src/harness/native/
 *  memory.rs::find_unique), which a distinct line always satisfies. */
function MemoryEditor() {
  const scope = useLearning((s) => s.memoryScope);
  const memory = useLearning((s) => s.memory);
  const memoryLoaded = useLearning((s) => s.memoryLoaded);
  const loadMemory = useLearning((s) => s.loadMemory);
  const addMemory = useLearning((s) => s.addMemory);
  const replaceMemory = useLearning((s) => s.replaceMemory);
  const removeMemory = useLearning((s) => s.removeMemory);

  const [newText, setNewText] = useState("");
  const [editingEntry, setEditingEntry] = useState<string | null>(null);
  const [editText, setEditText] = useState("");
  const [busy, setBusy] = useState(false);

  useEffect(() => {
    void loadMemory(scope);
  }, [scope, loadMemory]);

  const submitAdd = async () => {
    const text = newText.trim();
    if (!text) return;
    setBusy(true);
    const ok = await addMemory(scope, text);
    setBusy(false);
    if (ok) setNewText("");
  };

  const submitEdit = async () => {
    if (editingEntry === null) return;
    const text = editText.trim();
    if (!text) return;
    setBusy(true);
    const ok = await replaceMemory(scope, editingEntry, text);
    setBusy(false);
    if (ok) setEditingEntry(null);
  };

  const remove = async (entry: string) => {
    setBusy(true);
    await removeMemory(scope, entry);
    setBusy(false);
  };

  return (
    <SettingsCard>
      <SettingsCardHeader>
        <SettingsCardTitle>Memory</SettingsCardTitle>
        <Combobox
          aria-label="Memory scope"
          options={MEMORY_SCOPES.map((s) => ({ value: s, label: scopeLabel(s) }))}
          value={scope}
          onValueChange={(v) => v && void loadMemory(v as MemoryScope)}
          trigger={
            <Button type="button" variant="outline" size="sm" className="ml-auto gap-1.5">
              {scopeLabel(scope)}
              <ChevronDown aria-hidden size={11} strokeWidth={2} className="size-[11px]" />
            </Button>
          }
        />
      </SettingsCardHeader>
      <div className="max-h-[220px] overflow-y-auto">
        {!memoryLoaded ? (
          <div className="px-[18px] py-6 text-center text-[12.5px] text-muted-foreground">Loading…</div>
        ) : memory.length === 0 ? (
          <div className="px-[18px] py-6 text-center text-[12.5px] text-muted-foreground">No entries in this scope yet.</div>
        ) : (
          memory.map((entry, i) => (
            <div key={`${scope}:${i}:${entry}`} className="border-b border-border px-[18px] py-2.5 last:border-b-0">
              {editingEntry === entry ? (
                <div className="flex flex-col gap-2">
                  <Textarea value={editText} onChange={(e) => setEditText(e.target.value)} rows={2} className="resize-y" />
                  <div className="flex justify-end gap-1.5">
                    <Button type="button" size="xs" variant="ghost" onClick={() => setEditingEntry(null)}>
                      Cancel
                    </Button>
                    <Button type="button" size="xs" disabled={busy || !editText.trim()} onClick={() => void submitEdit()}>
                      Save
                    </Button>
                  </div>
                </div>
              ) : (
                <div className="flex items-start gap-1.5">
                  <span className="min-w-0 flex-1 whitespace-pre-wrap text-[12.5px] leading-[1.5]">{entry}</span>
                  <Button
                    type="button"
                    variant="ghost"
                    size="icon-xs"
                    title="Edit"
                    onClick={() => {
                      setEditingEntry(entry);
                      setEditText(entry);
                    }}
                  >
                    <Pencil aria-hidden size={12} strokeWidth={2} />
                  </Button>
                  <Button type="button" variant="ghost" size="icon-xs" title="Remove" disabled={busy} onClick={() => void remove(entry)}>
                    <Trash2 aria-hidden size={12} strokeWidth={2} />
                  </Button>
                </div>
              )}
            </div>
          ))
        )}
      </div>
      <div className="flex items-start gap-2 px-[18px] py-3">
        <Textarea
          value={newText}
          onChange={(e) => setNewText(e.target.value)}
          placeholder="Add a durable fact to this scope…"
          rows={2}
          className="resize-y"
        />
        <Button type="button" size="sm" disabled={busy || !newText.trim()} onClick={() => void submitAdd()}>
          Add
        </Button>
      </div>
    </SettingsCard>
  );
}

/** Full-screen Learning panel (Task 12): journey graph, self-improvement
 *  activity feed, curator status/rollback, and the memory editor — the
 *  Cockpit-side consumer of the Task-11 `learning_*`/`curator_*`/skill-usage
 *  commands. Skeleton mirrors views/InboxView.tsx. */
export function LearningView() {
  const graph = useLearning((s) => s.graph);
  const graphLoaded = useLearning((s) => s.graphLoaded);
  const skills = useLearning((s) => s.skills);
  const skillsLoaded = useLearning((s) => s.skillsLoaded);
  const curator = useLearning((s) => s.curator);
  const curatorLoaded = useLearning((s) => s.curatorLoaded);
  const rollingBack = useLearning((s) => s.rollingBack);
  const loadGraph = useLearning((s) => s.loadGraph);
  const loadSkills = useLearning((s) => s.loadSkills);
  const loadCurator = useLearning((s) => s.loadCurator);
  const rollbackCurator = useLearning((s) => s.rollbackCurator);

  useEffect(() => {
    if (!graphLoaded) void loadGraph();
    if (!skillsLoaded) void loadSkills();
    if (!curatorLoaded) void loadCurator();
  }, [graphLoaded, skillsLoaded, curatorLoaded, loadGraph, loadSkills, loadCurator]);

  return (
    <div className="flex min-h-0 flex-1 flex-col overflow-y-auto px-6 py-5">
      <div className="mx-auto w-full max-w-[880px]">
        <div className="mb-4 flex items-center gap-2">
          <Sparkles aria-hidden size={16} className="text-muted-foreground" />
          <h1 className="text-[15px] font-semibold">Learning</h1>
          <span className="text-xs text-muted-foreground">{skills.length} skills tracked</span>
        </div>

        <SettingsCard className="mb-3">
          <SettingsCardHeader>
            <SettingsCardTitle>Journey</SettingsCardTitle>
          </SettingsCardHeader>
          <div className="px-[18px] py-3.5">
            <JourneyGraph graph={graph} skillUsage={skills} />
          </div>
        </SettingsCard>

        <div className="mb-3 grid grid-cols-1 gap-3 lg:grid-cols-2">
          <ReviewFeed skills={skills} curatorRuns={curator?.recent ?? []} />
          <CuratorCard status={curator} rollingBack={rollingBack} onRollback={(id) => void rollbackCurator(id)} />
        </div>

        <MemoryEditor />
      </div>
    </div>
  );
}
