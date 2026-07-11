import { useState } from "react";
import { ChevronDown } from "lucide-react";
import { Button, MenuPanel, MenuPanelItem, MenuPanelSection, MenuPanelSeparator } from "@ryuzi/ui";
import type { ProjectRuntimeInfo, SelectableModelInfo, SessionRuntimeInfo } from "@/bindings";

type Props = {
  models: SelectableModelInfo[];
  runtime: ProjectRuntimeInfo | SessionRuntimeInfo | null;
  onChange: (model: string | null, effort: string | null) => void;
  disabled?: boolean;
  running?: boolean;
};

export function ComposerModelEffortMenu({ models, runtime, onChange, disabled, running }: Props) {
  const [open, setOpen] = useState(false);
  const selected = models.find((model) => model.requestValue === runtime?.model) ?? runtime?.modelInfo ?? null;
  const supported = selected?.supported ?? [];
  const stale = runtime?.storedEffortStatus === "unsupported";
  const unknown = runtime?.storedEffortStatus === "unknownMetadata";
  const effectiveLabel = runtime?.effectiveEffortLabel ?? runtime?.effectiveEffort;
  const resolvedDefault =
    selected?.supported.find((option) => option.value === selected.resolvedDefault)?.label ?? selected?.resolvedDefault;
  const modelDefaultLabel = `Model default${resolvedDefault ? ` · ${resolvedDefault}` : ""}`;
  const effortLabel = stale
    ? modelDefaultLabel
    : runtime?.storedEffort
      ? (supported.find((option) => option.value === runtime.storedEffort)?.label ?? runtime.storedEffort)
      : effectiveLabel
        ? modelDefaultLabel
        : modelDefaultLabel;

  const chooseModel = (model: SelectableModelInfo | null) => {
    const effort =
      model && runtime?.storedEffort && model.supported.some((option) => option.value === runtime.storedEffort)
        ? runtime.storedEffort
        : null;
    onChange(model?.requestValue ?? null, effort);
  };

  return (
    <div className="relative">
      <Button
        variant="ghost"
        size="sm"
        aria-label="Model and effort"
        disabled={disabled}
        onClick={() => setOpen((value) => !value)}
        className="max-w-[260px] font-semibold"
      >
        <span className="truncate">{selected?.displayName ?? runtime?.model ?? "Default model"}</span>
        {supported.length > 0 ? <span className="text-muted-foreground">· {effortLabel}</span> : null}
        <ChevronDown data-icon="inline-end" aria-hidden />
      </Button>
      {open ? (
        <MenuPanel onClose={() => setOpen(false)} className="bottom-full right-0 mb-1.5 w-[340px]">
          <MenuPanelSection>Model</MenuPanelSection>
          <MenuPanelItem selected={runtime?.model === null} onClick={() => chooseModel(null)}>
            <span className="flex-1">Default model</span>
          </MenuPanelItem>
          {models.map((model) => (
            <MenuPanelItem key={model.requestValue} selected={model.requestValue === runtime?.model} onClick={() => chooseModel(model)}>
              <span className="min-w-0 flex-1">
                <span className="block truncate font-medium">{model.displayName}</span>
                <span className="block truncate text-xs text-muted-foreground">{model.requestValue}</span>
              </span>
            </MenuPanelItem>
          ))}
          {supported.length > 0 ? (
            <>
              <MenuPanelSeparator />
              <MenuPanelSection>Effort</MenuPanelSection>
              {supported.length === 1 ? (
                <fieldset disabled data-readonly className="px-2.5 py-2">
                  <legend className="sr-only">Effort</legend>
                  <div className="font-medium">
                    {stale ? modelDefaultLabel : unknown ? (runtime?.storedEffort ?? modelDefaultLabel) : supported[0].label}
                  </div>
                  {stale ? (
                    <div className="text-xs text-destructive">{runtime?.storedEffort} is unsupported</div>
                  ) : unknown ? (
                    <div className="text-xs text-muted-foreground">Metadata unknown; stored value is preserved</div>
                  ) : supported[0].description ? (
                    <div className="text-xs text-muted-foreground">{supported[0].description}</div>
                  ) : null}
                  <div className="pt-1 text-xs text-muted-foreground">Read-only effort</div>
                </fieldset>
              ) : (
                <>
                  <MenuPanelItem selected={!runtime?.storedEffort || stale} onClick={() => onChange(runtime?.model ?? null, null)}>
                    <span className="min-w-0 flex-1">
                      <span className="block font-medium">{modelDefaultLabel}</span>
                      {stale ? (
                        <span className="block text-xs text-destructive">{runtime?.storedEffort} is unsupported</span>
                      ) : unknown ? (
                        <span className="block text-xs text-muted-foreground">Metadata unknown; stored value is preserved</span>
                      ) : null}
                    </span>
                  </MenuPanelItem>
                  {supported.map((option) => (
                    <MenuPanelItem
                      key={option.value}
                      selected={!stale && runtime?.storedEffort === option.value}
                      onClick={() => onChange(runtime?.model ?? null, option.value)}
                    >
                      <span className="min-w-0 flex-1">
                        <span className="block font-medium">{option.label}</span>
                        {option.description ? <span className="block text-xs text-muted-foreground">{option.description}</span> : null}
                      </span>
                    </MenuPanelItem>
                  ))}
                </>
              )}
            </>
          ) : unknown ? (
            <div className="px-2.5 py-2 text-xs text-muted-foreground">Metadata unknown; stored value is preserved</div>
          ) : null}
          {running ? <div className="px-2.5 py-2 text-xs text-muted-foreground">Changes apply to the next turns.</div> : null}
        </MenuPanel>
      ) : null}
    </div>
  );
}
