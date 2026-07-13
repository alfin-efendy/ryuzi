import { useEffect, useState } from "react";
import { Button, Combobox, SettingsCard, SettingsCardRow, SettingsCardTitle } from "@ryuzi/ui";
import type { AgentDetailInfo, AgentModelInfo } from "@/bindings";
import { ModelPicker } from "@/components/ModelPicker";
import { useAgents } from "@/store-agents";
import { mutationFromDetail } from "./agentMutation";

export function AgentModelTab({ detail }: { detail: AgentDetailInfo }) {
  const models = useAgents((state) => state.models);
  const saving = useAgents((state) => state.saving);
  const [model, setModel] = useState<AgentModelInfo>(detail.summary.model);

  useEffect(() => setModel(detail.summary.model), [detail]);

  const value = model.kind === "route" ? model.route : model.name;
  const info = models.find((candidate) => candidate.requestValue === value) ?? detail.modelInfo;
  const supported = model.kind === "concrete" ? (info?.supported ?? []) : [];

  const selectKind = (kind: string) => {
    if (kind === model.kind) return;
    const candidate = models.find((item) => (kind === "route" ? item.kind === "namedRoute" : item.kind === "concrete"));
    if (!candidate) return;
    setModel(
      kind === "route"
        ? { kind: "route", route: candidate.requestValue }
        : { kind: "concrete", name: candidate.requestValue, effort: null },
    );
  };

  const selectModel = (requestValue: string) => {
    const candidate = models.find((item) => item.requestValue === requestValue);
    if (candidate?.kind === "namedRoute") {
      setModel({ kind: "route", route: requestValue });
      return;
    }
    const effort = model.kind === "concrete" && candidate?.supported.some((option) => option.value === model.effort) ? model.effort : null;
    setModel({ kind: "concrete", name: requestValue, effort });
  };

  return (
    <SettingsCard>
      <div className="border-b border-border px-[18px] py-3.5">
        <SettingsCardTitle>Model assignment</SettingsCardTitle>
      </div>
      <SettingsCardRow className="gap-4">
        <span className="w-40 shrink-0 text-[13px] font-medium">Selection type</span>
        <Combobox
          aria-label="Agent model type"
          className="w-[190px]"
          options={[
            { value: "concrete", label: "Concrete model" },
            { value: "route", label: "Model route" },
          ]}
          value={model.kind}
          onValueChange={selectKind}
          disabled={saving}
        />
      </SettingsCardRow>
      <SettingsCardRow className="gap-4">
        <span className="w-40 shrink-0 text-[13px] font-medium">Model</span>
        <ModelPicker
          ariaLabel="Agent model"
          variant="field"
          models={models
            .filter((item) => (model.kind === "route" ? item.kind === "namedRoute" : item.kind === "concrete"))
            .map((item) => item.requestValue)}
          value={value}
          onValueChange={selectModel}
          disabled={saving}
        />
        {supported.length > 0 && model.kind === "concrete" ? (
          <Combobox
            aria-label="Agent effort"
            className="w-[170px]"
            options={[
              { value: "", label: "Model default" },
              ...supported.map((option) => ({ value: option.value, label: option.label, description: option.description ?? undefined })),
            ]}
            value={model.effort ?? ""}
            onValueChange={(effort) => setModel({ ...model, effort: effort || null })}
            disabled={saving}
          />
        ) : null}
      </SettingsCardRow>
      <div className="flex justify-end px-[18px] py-3">
        <Button
          onClick={() => void useAgents.getState().update(detail.summary.id, { ...mutationFromDetail(detail), model })}
          disabled={saving}
        >
          Save model
        </Button>
      </div>
    </SettingsCard>
  );
}
