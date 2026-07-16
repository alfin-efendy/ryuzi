import { useEffect, useMemo, useRef, useState } from "react";
import { Edit3, Plus, Search, Trash2 } from "lucide-react";
import { Button, Combobox, FormField, Input, Modal, ModalBody, ModalFooter, ModalHeader, SettingsCard, Switch, Textarea } from "@ryuzi/ui";
import type { CommandInfo, Project, ProjectCommandInfo, ProjectCommandInputDto, ProjectCommandMutationDto } from "@/bindings";
import { ConfirmActionModal } from "@/components/modals/ConfirmActionModal";
import { LOCAL_RUNNER } from "@/lib/session-key";
import { useAgents } from "@/store-agents";
import { useNative, type ProjectCommandMutationResult } from "@/store-native";
import { useStore } from "@/store";

const NAME_ALLOWED = /^[a-z0-9_-]+(?:\/[a-z0-9_-]+)*$/;
const RESERVED_NAMES = new Set(["init", "review", "compact"]);

export function projectCommandNameError(name: string, editing = false): string | null {
  if (name.length === 0 || name.length > 80) return "Name must contain 1 through 80 characters.";
  if (!NAME_ALLOWED.test(name)) return "Use lowercase letters, digits, '-', '_', and '/' only.";
  if (!editing && RESERVED_NAMES.has(name)) return "Built-in commands cannot be created or updated.";
  return null;
}

export function projectCommandPreview(name: string, template: string): string {
  const invocation = `/${name || "command"} <arguments>`;
  const body = template
    .replace(/\$ARGUMENTS/g, "<arguments>")
    .replace(/\$([1-9])/g, (_match: string, index: string) => `<argument ${index}>`);
  return `${invocation}\n${body}`;
}

type CommandDraft = ProjectCommandInputDto;

function blankDraft(): CommandDraft {
  return { name: "", description: "", template: "", agent: null, model: null, subtask: false };
}

function draftFor(command: ProjectCommandInfo): CommandDraft {
  const { name, description, template, agent, model, subtask } = command;
  return { name, description, template, agent, model, subtask };
}

function CommandEditor({
  command,
  agentOptions,
  modelOptions,
  onClose,
  onSave,
}: {
  command: ProjectCommandInfo | null;
  agentOptions: { value: string; label: string; description?: string }[];
  modelOptions: { value: string; label: string }[];
  onClose: () => void;
  onSave: (draft: CommandDraft) => Promise<ProjectCommandMutationResult>;
}) {
  const [draft, setDraft] = useState<CommandDraft>(() => (command ? draftFor(command) : blankDraft()));
  const [saving, setSaving] = useState(false);
  const descriptionRef = useRef<HTMLInputElement>(null);
  const nameError = projectCommandNameError(draft.name, command !== null);
  const valid = !nameError && draft.template.trim().length > 0;

  const save = async () => {
    if (!valid || saving) return;
    setSaving(true);
    const result = await onSave({
      ...draft,
      name: draft.name.trim(),
      description: draft.description.trim(),
      template: draft.template.trim(),
    });
    setSaving(false);
    if (result.status === "success" || result.status === "conflict") onClose();
  };

  return (
    <Modal onClose={onClose} width={560} busy={saving} initialFocus={command ? descriptionRef : undefined}>
      <ModalHeader
        title={command ? "Edit command" : "New command"}
        description="Project commands are available only inside this project's local runner."
      />
      <ModalBody className="flex flex-col gap-4">
        <FormField label="Name" hint={nameError ?? "Lowercase path, for example team/review."}>
          <Input
            aria-label="Name"
            value={draft.name}
            disabled={!!command}
            onChange={(event) => setDraft((current) => ({ ...current, name: event.target.value }))}
            placeholder="review"
          />
        </FormField>
        <FormField label="Description" hint="Optional summary shown in the command list.">
          <Input
            ref={descriptionRef}
            aria-label="Description"
            value={draft.description}
            onChange={(event) => setDraft((current) => ({ ...current, description: event.target.value }))}
            placeholder="Review the current change"
          />
        </FormField>
        <FormField label="Template" hint="Use $ARGUMENTS for all arguments or $1 through $9 for positional arguments.">
          <Textarea
            aria-label="Template"
            value={draft.template}
            onChange={(event) => setDraft((current) => ({ ...current, template: event.target.value }))}
            placeholder="Review $ARGUMENTS"
            rows={6}
          />
        </FormField>
        <div className="rounded-lg border border-border bg-muted/30 px-3 py-2">
          <div className="text-[11px] font-semibold uppercase tracking-[0.04em] text-muted-foreground">Preview</div>
          <pre className="mt-1 whitespace-pre-wrap font-mono text-xs leading-5 text-foreground">
            {projectCommandPreview(draft.name, draft.template)}
          </pre>
        </div>
        <div className="grid grid-cols-2 gap-3">
          <FormField label="Agent" hint="Optional agent override.">
            {agentOptions.length > 0 ? (
              <Combobox
                aria-label="Agent"
                options={agentOptions}
                value={draft.agent}
                onValueChange={(agent) => setDraft((current) => ({ ...current, agent }))}
                placeholder="Project default"
              />
            ) : (
              <p className="h-8 py-2 text-xs text-muted-foreground">No agents available for this project.</p>
            )}
          </FormField>
          <FormField label="Model" hint="Optional model override.">
            {modelOptions.length > 0 ? (
              <Combobox
                aria-label="Model"
                options={modelOptions}
                value={draft.model}
                onValueChange={(model) => setDraft((current) => ({ ...current, model }))}
                placeholder="Project default"
              />
            ) : (
              <p className="h-8 py-2 text-xs text-muted-foreground">No models available.</p>
            )}
          </FormField>
        </div>
        <div className="flex items-center justify-between gap-3 rounded-lg border border-border px-3 py-2.5">
          <div>
            <div className="text-xs font-semibold">Run as subtask</div>
            <div className="mt-0.5 text-xs text-muted-foreground">Start the command in an isolated subtask.</div>
          </div>
          <Switch
            on={draft.subtask ?? false}
            onToggle={() => setDraft((current) => ({ ...current, subtask: !(current.subtask ?? false) }))}
            label="Run as subtask"
          />
        </div>
      </ModalBody>
      <ModalFooter>
        <Button variant="outline" onClick={onClose} disabled={saving}>
          Cancel
        </Button>
        <Button onClick={() => void save()} disabled={!valid || saving}>
          {saving ? "Saving…" : command ? "Save" : "Create"}
        </Button>
      </ModalFooter>
    </Modal>
  );
}

function OriginBadge({ origin }: { origin: CommandInfo["origin"] }) {
  const label = origin === "builtin" ? "Built-in" : origin === "global" ? "Global" : "Project";
  return <span className="rounded bg-muted px-1.5 py-0.5 text-[10px] font-medium text-muted-foreground">{label}</span>;
}

function commandStatus(
  command: Pick<CommandInfo, "origin" | "effective" | "shadowsGlobal">,
  effectiveOrigin: CommandInfo["origin"] | undefined,
) {
  if (command.effective) return command.origin === "project" && command.shadowsGlobal ? "Overrides global" : "Effective";
  const source = effectiveOrigin === "builtin" ? "built-in" : (effectiveOrigin ?? "higher-precedence source");
  return `Shadowed by ${source}`;
}

function StatusBadge({
  command,
  effectiveOrigin,
}: {
  command: Pick<CommandInfo, "origin" | "effective" | "shadowsGlobal">;
  effectiveOrigin: CommandInfo["origin"] | undefined;
}) {
  return (
    <span className="rounded bg-muted px-1.5 py-0.5 text-[10px] font-medium text-muted-foreground">
      {commandStatus(command, effectiveOrigin)}
    </span>
  );
}

function ReadOnlyCommandRow({ command, effectiveOrigin }: { command: CommandInfo; effectiveOrigin: CommandInfo["origin"] | undefined }) {
  return (
    <SettingsCard className="flex min-h-[88px] items-stretch">
      <div className="min-w-0 flex-1 px-[18px] py-3">
        <div className="flex items-center gap-2">
          <span className="font-mono text-[13px] font-semibold">/{command.name}</span>
          <OriginBadge origin={command.origin} />
          <StatusBadge command={command} effectiveOrigin={effectiveOrigin} />
          {command.subtask ? (
            <span className="rounded bg-muted px-1.5 py-0.5 text-[10px] font-medium text-muted-foreground">Subtask</span>
          ) : null}
        </div>
        {command.description ? <p className="mt-1 truncate text-xs text-muted-foreground">{command.description}</p> : null}
        {(command.agent || command.model) && (
          <p className="mt-1 text-[11px] text-muted-foreground">
            {[command.agent && `Agent: ${command.agent}`, command.model && `Model: ${command.model}`].filter(Boolean).join(" · ")}
          </p>
        )}
      </div>
    </SettingsCard>
  );
}

function CommandRow({
  command,
  catalogCommand,
  effectiveOrigin,
  onEdit,
  onDelete,
}: {
  command: ProjectCommandInfo;
  catalogCommand: CommandInfo | undefined;
  effectiveOrigin: CommandInfo["origin"] | undefined;
  onEdit: () => void;
  onDelete: (trigger: HTMLButtonElement) => void;
}) {
  return (
    <SettingsCard className="flex min-h-[88px] items-stretch">
      <div className="min-w-0 flex-1 px-[18px] py-3">
        <div className="flex items-center gap-2">
          <span className="font-mono text-[13px] font-semibold">/{command.name}</span>
          <OriginBadge origin="project" />
          {catalogCommand ? <StatusBadge command={catalogCommand} effectiveOrigin={effectiveOrigin} /> : null}
          {command.subtask ? (
            <span className="rounded bg-muted px-1.5 py-0.5 text-[10px] font-medium text-muted-foreground">Subtask</span>
          ) : null}
        </div>
        {command.description ? <p className="mt-1 truncate text-xs text-muted-foreground">{command.description}</p> : null}
        <p className="mt-1.5 truncate font-mono text-[11px] text-muted-foreground">{command.template}</p>
        {(command.agent || command.model) && (
          <p className="mt-1 text-[11px] text-muted-foreground">
            {[command.agent && `Agent: ${command.agent}`, command.model && `Model: ${command.model}`].filter(Boolean).join(" · ")}
          </p>
        )}
      </div>
      <div className="flex shrink-0 items-center gap-1 border-l border-border px-2">
        <Button variant="ghost" size="icon" aria-label={`Edit /${command.name}`} onClick={onEdit}>
          <Edit3 aria-hidden size={15} />
        </Button>
        <Button variant="ghost" size="icon" aria-label={`Delete /${command.name}`} onClick={(event) => onDelete(event.currentTarget)}>
          <Trash2 aria-hidden size={15} />
        </Button>
      </div>
    </SettingsCard>
  );
}

export function CommandsTab({ projects, defaultProjectId }: { projects?: Project[]; defaultProjectId?: string | null }) {
  const storeProjects = useStore((state) => state.projects);
  const selectedProjectId = useStore((state) => state.selectedProjectId);
  const availableProjects = projects ?? storeProjects;
  const [projectId, setProjectId] = useState<string | null>(() => (defaultProjectId === undefined ? selectedProjectId : defaultProjectId));
  const [search, setSearch] = useState("");
  const [editing, setEditing] = useState<ProjectCommandInfo | null | undefined>(undefined);
  const [deleting, setDeleting] = useState<{ command: ProjectCommandInfo; trigger: HTMLButtonElement } | null>(null);
  const commands = useNative((state) => (projectId ? state.projectCommandsByProject[projectId] : undefined));
  const effectiveCommands = useNative((state) => (projectId ? state.commandsByProject[projectId] : undefined));
  const projectAgents = useNative((state) => (projectId ? state.agentsByProject[projectId] : undefined));
  const agentModels = useAgents((state) => state.models);

  useEffect(() => {
    if (defaultProjectId !== undefined) setProjectId(defaultProjectId);
  }, [defaultProjectId]);
  useEffect(() => {
    if (defaultProjectId === undefined && !projectId && selectedProjectId) setProjectId(selectedProjectId);
  }, [defaultProjectId, projectId, selectedProjectId]);
  useEffect(() => {
    if (projectId) {
      void useNative.getState().loadProjectCommands(LOCAL_RUNNER, projectId);
      void useNative.getState().loadCommands(LOCAL_RUNNER, projectId);
      void useNative.getState().loadAgents(LOCAL_RUNNER, projectId);
    }
  }, [projectId]);

  const filteredCommands = useMemo(() => {
    const term = search.trim().toLowerCase();
    if (!term) return commands ?? [];
    return (commands ?? []).filter((command) =>
      [command.name, command.description, command.template, command.agent, command.model, command.subtask ? "subtask" : ""].some((value) =>
        value?.toLowerCase().includes(term),
      ),
    );
  }, [commands, search]);
  const externalCommands = useMemo(() => (effectiveCommands ?? []).filter((command) => command.origin !== "project"), [effectiveCommands]);
  const projectCommandsByName = useMemo(
    () => new Map((effectiveCommands ?? []).filter((command) => command.origin === "project").map((command) => [command.name, command])),
    [effectiveCommands],
  );
  const effectiveOriginByName = useMemo(
    () => new Map((effectiveCommands ?? []).filter((command) => command.effective).map((command) => [command.name, command.origin])),
    [effectiveCommands],
  );
  const projectOptions = availableProjects.map((project) => ({
    value: project.projectId,
    label: project.name,
    description: project.workdir,
  }));
  const selectedProject = availableProjects.find((project) => project.projectId === projectId) ?? null;
  const agentOptions = (projectAgents ?? []).map((agent) => ({ value: agent.name, label: agent.name, description: agent.description }));
  const modelOptions = agentModels.map((model) => ({ value: model.requestValue, label: model.displayName }));

  const save = async (draft: CommandDraft): Promise<ProjectCommandMutationResult> => {
    if (!projectId) return { status: "error", message: "Select a project before saving a command." };
    if (editing) {
      const { name: _, ...input } = draft;
      return useNative.getState().updateProjectCommand(LOCAL_RUNNER, projectId, editing, input as ProjectCommandMutationDto);
    }
    return useNative.getState().createProjectCommand(LOCAL_RUNNER, projectId, draft);
  };
  const confirmDelete = async (): Promise<boolean> => {
    if (!deleting || !projectId) return false;
    const result = await useNative.getState().deleteProjectCommand(LOCAL_RUNNER, projectId, deleting.command);
    if (result.status === "success" || result.status === "conflict") {
      setDeleting(null);
      return true;
    }
    return false;
  };

  return (
    <div className="min-h-0 flex-1 overflow-y-auto px-8 py-7">
      <div className="mx-auto flex max-w-[860px] flex-col gap-5">
        <div className="flex flex-wrap items-end justify-between gap-3">
          <div className="min-w-[260px] flex-1">
            <FormField label="Project">
              <Combobox
                aria-label="Project"
                options={projectOptions}
                value={projectId}
                onValueChange={setProjectId}
                placeholder="Select a project"
              />
            </FormField>
          </div>
          <Button onClick={() => setEditing(null)} disabled={!selectedProject}>
            <Plus aria-hidden size={15} /> New command
          </Button>
        </div>

        <SettingsCard className="px-[18px] py-3 text-xs text-muted-foreground">
          Project commands are editable. Global and built-in command sources are read-only; status badges show which source executes.
        </SettingsCard>

        {selectedProject ? (
          <>
            <div className="relative">
              <Search
                aria-hidden
                size={15}
                className="pointer-events-none absolute left-3 top-1/2 -translate-y-1/2 text-muted-foreground"
              />
              <Input
                aria-label="Search project commands"
                value={search}
                onChange={(event) => setSearch(event.target.value)}
                placeholder="Search commands"
                className="pl-9"
              />
            </div>
            <div className="flex flex-col gap-2">
              {commands === undefined ? (
                <SettingsCard className="p-6 text-center text-[13px] text-muted-foreground">Loading project commands…</SettingsCard>
              ) : filteredCommands.length > 0 ? (
                filteredCommands.map((command) => (
                  <CommandRow
                    key={command.name}
                    command={command}
                    catalogCommand={projectCommandsByName.get(command.name)}
                    effectiveOrigin={effectiveOriginByName.get(command.name)}
                    onEdit={() => setEditing(command)}
                    onDelete={(trigger) => setDeleting({ command, trigger })}
                  />
                ))
              ) : (
                <SettingsCard className="p-6 text-center text-[13px] text-muted-foreground">
                  {search ? "No project commands match your search." : "No project commands yet."}
                </SettingsCard>
              )}
            </div>
            <div className="flex flex-col gap-2">
              <div className="text-xs font-semibold text-muted-foreground">Global and built-in commands</div>
              {effectiveCommands === undefined ? (
                <SettingsCard className="p-6 text-center text-[13px] text-muted-foreground">
                  Loading global and built-in commands…
                </SettingsCard>
              ) : externalCommands.length > 0 ? (
                externalCommands.map((command) => (
                  <ReadOnlyCommandRow
                    key={`${command.origin}:${command.name}`}
                    command={command}
                    effectiveOrigin={effectiveOriginByName.get(command.name)}
                  />
                ))
              ) : (
                <SettingsCard className="p-6 text-center text-[13px] text-muted-foreground">No global commands.</SettingsCard>
              )}
            </div>
          </>
        ) : (
          <SettingsCard className="p-6 text-center text-[13px] text-muted-foreground">
            Select a project to manage project commands
          </SettingsCard>
        )}
      </div>
      {editing !== undefined && (
        <CommandEditor
          command={editing}
          agentOptions={agentOptions}
          modelOptions={modelOptions}
          onClose={() => setEditing(undefined)}
          onSave={save}
        />
      )}
      <ConfirmActionModal
        open={deleting !== null}
        title={deleting ? `Delete /${deleting.command.name}?` : "Delete command?"}
        description="This project command will be permanently deleted."
        confirmLabel="Delete command"
        busyLabel="Deleting…"
        trigger={deleting?.trigger ?? null}
        onClose={() => setDeleting(null)}
        onConfirm={confirmDelete}
      />
    </div>
  );
}
