import { useEffect, useMemo, useState } from "react";
import { Pencil, Plus, Trash2, Wrench } from "lucide-react";
import {
  Badge,
  Button,
  Combobox,
  FormField,
  Input,
  Modal,
  ModalBody,
  ModalFooter,
  ModalHeader,
  SettingsCard,
  SettingsCardHeader,
  SettingsCardHint,
  SettingsCardRow,
  SettingsCardTitle,
  Textarea,
} from "@ryuzi/ui";
import type {
  CuratorHistorySnapshotInfo,
  InvalidKnowledgeConceptInfo,
  KnowledgeConceptInfo,
  KnowledgeConceptMutationInfo,
} from "@/bindings";
import { JourneyGraph } from "@/components/learning/JourneyGraph";
import { ReviewFeed } from "@/components/learning/ReviewFeed";
import { CuratorCard } from "@/components/learning/CuratorCard";
import { projectLabel } from "@/lib/sidebar";
import { useStore } from "@/store";
import { useLearning } from "@/store-learning";

const SCOPE_OPTIONS = [
  { value: "global", label: "Global" },
  { value: "user", label: "User" },
  { value: "project", label: "Project" },
];
const emptyDraft: KnowledgeConceptMutationInfo = { title: "", description: "", body: "", scope: "user", projectId: null, tags: [] };

type Confirmation =
  | { kind: "concept"; concept: KnowledgeConceptInfo }
  | { kind: "invalid"; invalid: InvalidKnowledgeConceptInfo }
  | { kind: "rollback"; snapshot: CuratorHistorySnapshotInfo };

export function AgentLearningTab({ agentId }: { agentId: string }) {
  const snapshot = useLearning((state) => state.byAgent[agentId]);
  const loading = useLearning((state) => state.loading[agentId] ?? false);
  const rollingBack = useLearning((state) => state.rollingBack[agentId] ?? null);
  const projects = useStore((state) => state.projects);
  const [editing, setEditing] = useState<KnowledgeConceptInfo | "new" | null>(null);
  const [draft, setDraft] = useState<KnowledgeConceptMutationInfo>(emptyDraft);
  const [repair, setRepair] = useState<InvalidKnowledgeConceptInfo | null>(null);
  const [raw, setRaw] = useState("");
  const [validatedRaw, setValidatedRaw] = useState<string | null>(null);
  const [confirmation, setConfirmation] = useState<Confirmation | null>(null);
  const projectOptions = useMemo(() => projects.map((project) => ({ value: project.projectId, label: projectLabel(project) })), [projects]);

  useEffect(() => {
    if (!snapshot && !loading) void useLearning.getState().load(agentId);
  }, [agentId, loading, snapshot]);

  const openEditor = (concept?: KnowledgeConceptInfo) => {
    setEditing(concept ?? "new");
    setDraft(
      concept
        ? {
            title: concept.title,
            description: concept.description,
            body: concept.body,
            scope: concept.scope ?? "user",
            projectId: concept.scope === "project" ? concept.projectId : null,
            tags: concept.tags,
          }
        : emptyDraft,
    );
  };
  const save = async () => {
    if (!editing || !draft.title.trim() || !draft.body.trim() || (draft.scope === "project" && !draft.projectId)) return;
    const input = {
      ...draft,
      title: draft.title.trim(),
      description: draft.description.trim(),
      body: draft.body.trim(),
      projectId: draft.scope === "project" ? draft.projectId : null,
    };
    const ok =
      editing === "new"
        ? await useLearning.getState().createConcept(agentId, input)
        : await useLearning.getState().updateConcept(agentId, editing.id, input);
    if (ok) setEditing(null);
  };
  const openRepair = (invalid: InvalidKnowledgeConceptInfo) => {
    setRepair(invalid);
    setRaw(invalid.rawMarkdown);
    setValidatedRaw(null);
  };
  const validate = async () => {
    if (!repair) return;
    const candidate = raw;
    const parsed = await useLearning.getState().validateRaw(agentId, repair.relativePath, candidate);
    setValidatedRaw(parsed ? candidate : null);
  };
  const replace = async () => {
    if (!repair || validatedRaw !== raw) return;
    if (await useLearning.getState().replaceRaw(agentId, repair.relativePath, raw)) setRepair(null);
  };
  const confirm = async () => {
    if (!confirmation) return;
    const ok =
      confirmation.kind === "concept"
        ? await useLearning.getState().deleteConcept(agentId, confirmation.concept.id)
        : confirmation.kind === "invalid"
          ? await useLearning.getState().deleteInvalid(agentId, confirmation.invalid.relativePath)
          : await useLearning.getState().rollback(agentId, confirmation.snapshot.snapshotId);
    if (ok) setConfirmation(null);
  };

  if (!snapshot)
    return <div className="py-10 text-center text-xs text-muted-foreground">{loading ? "Loading Learning…" : "Learning unavailable."}</div>;

  return (
    <div className="flex flex-col gap-4">
      <section aria-labelledby="memory-heading">
        <SettingsCard>
          <SettingsCardHeader>
            <SettingsCardTitle>Memory</SettingsCardTitle>
            <span className="ml-auto text-xs text-muted-foreground">{snapshot.concepts.length}</span>
            <Button type="button" size="sm" onClick={() => openEditor()}>
              <Plus aria-hidden size={12} /> Add memory
            </Button>
          </SettingsCardHeader>
          {snapshot.concepts.length === 0 ? (
            <div className="px-[18px] py-6 text-center text-xs text-muted-foreground">No memory concepts yet.</div>
          ) : (
            snapshot.concepts.map((concept) => (
              <SettingsCardRow key={concept.id} className="items-start">
                <div className="min-w-0 flex-1">
                  <div className="flex items-center gap-2 text-xs font-medium">
                    {concept.title} {concept.scope ? <Badge variant="outline">{concept.scope}</Badge> : null}
                  </div>
                  <SettingsCardHint>{concept.description || concept.body}</SettingsCardHint>
                </div>
                <Button
                  type="button"
                  variant="ghost"
                  size="icon-xs"
                  aria-label={`Edit ${concept.title}`}
                  onClick={() => openEditor(concept)}
                >
                  <Pencil aria-hidden size={12} />
                </Button>
                <Button
                  type="button"
                  variant="ghost"
                  size="icon-xs"
                  aria-label={`Delete ${concept.title}`}
                  onClick={() => setConfirmation({ kind: "concept", concept })}
                >
                  <Trash2 aria-hidden size={12} />
                </Button>
              </SettingsCardRow>
            ))
          )}
        </SettingsCard>
      </section>

      <section aria-labelledby="journey-heading">
        <h3 id="journey-heading" className="mb-2 text-[13px] font-semibold">
          Journey
        </h3>
        <JourneyGraph milestones={snapshot.journey} />
      </section>

      <section aria-labelledby="usage-heading">
        <SettingsCard>
          <SettingsCardHeader>
            <SettingsCardTitle>Skill usage</SettingsCardTitle>
          </SettingsCardHeader>
          {snapshot.skillUsage.length === 0 ? (
            <div className="px-[18px] py-6 text-center text-xs text-muted-foreground">No skill usage tied to knowledge yet.</div>
          ) : (
            snapshot.skillUsage.map((usage) => (
              <SettingsCardRow key={`${usage.skillId}:${usage.conceptId}`}>
                <span className="min-w-0 flex-1 truncate font-mono text-xs">{usage.skillId}</span>
                <span className="text-[11px] text-muted-foreground">
                  {usage.successes}/{usage.uses} successful
                </span>
              </SettingsCardRow>
            ))
          )}
        </SettingsCard>
      </section>

      <section aria-labelledby="reviews-heading">
        <h3 id="reviews-heading" className="mb-2 text-[13px] font-semibold">
          Reviews
        </h3>
        <ReviewFeed reviews={snapshot.reviews} />
      </section>

      <section aria-labelledby="curator-heading">
        <h3 id="curator-heading" className="mb-2 text-[13px] font-semibold">
          Curator
        </h3>
        <CuratorCard
          curator={snapshot.curator}
          history={snapshot.curatorHistory}
          rollingBack={rollingBack}
          onRollback={(value) => setConfirmation({ kind: "rollback", snapshot: value })}
        />
      </section>

      <section aria-labelledby="repair-heading">
        <SettingsCard>
          <SettingsCardHeader>
            <SettingsCardTitle>Repair knowledge</SettingsCardTitle>
          </SettingsCardHeader>
          {snapshot.invalid.length === 0 ? (
            <div className="px-[18px] py-6 text-center text-xs text-muted-foreground">No invalid knowledge files.</div>
          ) : (
            snapshot.invalid.map((invalid) => (
              <SettingsCardRow key={invalid.relativePath} className="items-start">
                <Wrench aria-hidden size={14} className="mt-0.5 shrink-0 text-destructive" />
                <div className="min-w-0 flex-1">
                  <div className="break-all font-mono text-xs">{invalid.relativePath}</div>
                  <SettingsCardHint>{invalid.error}</SettingsCardHint>
                </div>
                <Button
                  type="button"
                  variant="outline"
                  size="sm"
                  aria-label={`Repair ${invalid.relativePath}`}
                  onClick={() => openRepair(invalid)}
                >
                  Edit
                </Button>
                <Button
                  type="button"
                  variant="ghost"
                  size="sm"
                  onClick={() => void useLearning.getState().validateRaw(agentId, invalid.relativePath, invalid.rawMarkdown)}
                >
                  Validate
                </Button>
                <Button type="button" variant="ghost" size="sm" onClick={() => setConfirmation({ kind: "invalid", invalid })}>
                  Delete
                </Button>
              </SettingsCardRow>
            ))
          )}
        </SettingsCard>
      </section>

      {editing ? (
        <Modal onClose={() => setEditing(null)} width={520}>
          <ModalHeader title={editing === "new" ? "Add memory" : "Edit memory"} />
          <ModalBody className="flex flex-col gap-3">
            <FormField label="Title">
              <Input value={draft.title} onChange={(event) => setDraft({ ...draft, title: event.target.value })} />
            </FormField>
            <FormField label="Description">
              <Input value={draft.description} onChange={(event) => setDraft({ ...draft, description: event.target.value })} />
            </FormField>
            <FormField label="Body">
              <Textarea
                aria-label="Body"
                rows={6}
                value={draft.body}
                onChange={(event) => setDraft({ ...draft, body: event.target.value })}
              />
            </FormField>
            <FormField label="Scope">
              <Combobox
                aria-label="Scope"
                options={SCOPE_OPTIONS}
                value={draft.scope}
                onValueChange={(scope) => setDraft({ ...draft, scope, projectId: scope === "project" ? draft.projectId : null })}
              />
            </FormField>
            {draft.scope === "project" ? (
              <FormField label="Project">
                <Combobox
                  aria-label="Project"
                  options={projectOptions}
                  value={draft.projectId ?? ""}
                  onValueChange={(projectId) => setDraft({ ...draft, projectId })}
                  placeholder="Choose project…"
                />
              </FormField>
            ) : null}
            <FormField label="Tags" hint="Comma-separated">
              <Input
                value={draft.tags.join(", ")}
                onChange={(event) =>
                  setDraft({
                    ...draft,
                    tags: event.target.value
                      .split(",")
                      .map((tag) => tag.trim())
                      .filter(Boolean),
                  })
                }
              />
            </FormField>
          </ModalBody>
          <ModalFooter>
            <Button variant="outline" onClick={() => setEditing(null)}>
              Cancel
            </Button>
            <Button
              disabled={!draft.title.trim() || !draft.body.trim() || (draft.scope === "project" && !draft.projectId)}
              onClick={() => void save()}
            >
              Save memory
            </Button>
          </ModalFooter>
        </Modal>
      ) : null}

      {repair ? (
        <Modal onClose={() => setRepair(null)} width={620}>
          <ModalHeader title={`Repair ${repair.relativePath}`} description={repair.error} />
          <ModalBody>
            <FormField label="Raw Markdown">
              <Textarea aria-label="Raw Markdown" rows={16} value={raw} onChange={(event) => setRaw(event.target.value)} />
            </FormField>
          </ModalBody>
          <ModalFooter>
            <Button variant="outline" onClick={() => setRepair(null)}>
              Cancel
            </Button>
            <Button variant="outline" onClick={() => void validate()}>
              Validate
            </Button>
            <Button disabled={validatedRaw !== raw} onClick={() => void replace()}>
              Replace file
            </Button>
          </ModalFooter>
        </Modal>
      ) : null}

      {confirmation ? (
        <Modal onClose={() => setConfirmation(null)} width={460}>
          <ModalHeader title={confirmation.kind === "rollback" ? "Restore knowledge snapshot" : "Delete knowledge"} />
          <ModalBody>
            <p className="m-0 text-xs text-muted-foreground">
              {confirmation.kind === "rollback"
                ? `Restore knowledge snapshot ${confirmation.snapshot.concept.title}? Agent YAML and transcripts are not changed. The restored OKF state is recorded as a new rollback event.`
                : confirmation.kind === "concept"
                  ? `Delete ${confirmation.concept.title}? This cannot be undone.`
                  : `Delete invalid knowledge file ${confirmation.invalid.relativePath}? This cannot be undone.`}
            </p>
          </ModalBody>
          <ModalFooter>
            <Button variant="outline" onClick={() => setConfirmation(null)}>
              Cancel
            </Button>
            <Button variant={confirmation.kind === "rollback" ? "default" : "destructive"} onClick={() => void confirm()}>
              {confirmation.kind === "rollback" ? "Restore snapshot" : "Delete"}
            </Button>
          </ModalFooter>
        </Modal>
      ) : null}
    </div>
  );
}
