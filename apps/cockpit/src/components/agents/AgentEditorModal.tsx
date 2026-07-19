import { useEffect, useRef, useState } from "react";
import { Button, Combobox, FormField, Input, Modal, ModalBody, ModalFooter, ModalHeader, Textarea } from "@ryuzi/ui";
import type { AgentMutationInfo, AgentRegistryInfo } from "@/bindings";
import { useAgents } from "@/store-agents";
import { useNav } from "@/store-nav";

const COLOR_OPTIONS = [
  { value: "violet", label: "Violet" },
  { value: "blue", label: "Blue" },
  { value: "cyan", label: "Cyan" },
  { value: "emerald", label: "Emerald" },
  { value: "amber", label: "Amber" },
  { value: "rose", label: "Rose" },
];

function initialMutation(registry: AgentRegistryInfo): AgentMutationInfo {
  return {
    name: "",
    description: "",
    avatarColor: "violet",
    model: registry.subagentModel,
    personality: { preset: "helpful", custom: null },
    permissionMode: "ask",
    permissionRules: [],
    skills: [],
    nativeTools: [],
    pluginTools: [],
    apps: [],
  };
}

export function AgentEditorModal({ open, onClose }: { open: boolean; onClose: () => void }) {
  const registry = useAgents((s) => s.registry);
  const saving = useAgents((s) => s.saving);
  const [draft, setDraft] = useState<AgentMutationInfo | null>(() => (registry ? initialMutation(registry) : null));
  const nameRef = useRef<HTMLInputElement>(null);
  const nav = useNav();

  useEffect(() => {
    if (open && registry) setDraft(initialMutation(registry));
  }, [open, registry]);

  if (!open || !draft) return null;
  const valid = draft.name.trim().length > 0 && draft.description.trim().length > 0;

  const create = async () => {
    if (!valid || saving) return;
    const created = await useAgents.getState().create({
      ...draft,
      name: draft.name.trim(),
      description: draft.description.trim(),
    });
    if (!created) return;
    onClose();
    nav.navigate({ kind: "agentDetail", agentId: created.summary.id });
  };

  return (
    <Modal onClose={onClose} width={480} busy={saving} initialFocus={nameRef}>
      <ModalHeader title="New agent" description="Create a persistent main agent with isolated configuration and knowledge." />
      <ModalBody className="flex flex-col gap-4">
        <FormField label="Name">
          <Input
            ref={nameRef}
            value={draft.name}
            onChange={(event) => setDraft((current) => (current ? { ...current, name: event.target.value } : current))}
            placeholder="Reviewer"
          />
        </FormField>
        <FormField label="Description" hint="Explain the agent's role and operating focus.">
          <Textarea
            aria-label="Description"
            value={draft.description}
            onChange={(event) => setDraft((current) => (current ? { ...current, description: event.target.value } : current))}
            placeholder="Reviews implementation quality and regressions."
            rows={3}
          />
        </FormField>
        <FormField label="Avatar color">
          <Combobox
            aria-label="Avatar color"
            options={COLOR_OPTIONS}
            value={draft.avatarColor}
            onValueChange={(avatarColor) => setDraft((current) => (current ? { ...current, avatarColor } : current))}
          />
        </FormField>
      </ModalBody>
      <ModalFooter>
        <Button variant="outline" onClick={onClose} disabled={saving}>
          Cancel
        </Button>
        <Button onClick={() => void create()} disabled={!valid || saving}>
          {saving ? "Creating…" : "Create"}
        </Button>
      </ModalFooter>
    </Modal>
  );
}
