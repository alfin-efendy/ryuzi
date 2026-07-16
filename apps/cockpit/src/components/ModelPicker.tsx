import { useMemo } from "react";
import { ChevronDown } from "lucide-react";
import { Button, Combobox, type ComboboxGroup, type ComboboxOption } from "@ryuzi/ui";
import { groupModelOptions, withLeadingOption } from "@/lib/model-groups";
import { useConnections } from "@/store-connections";
import { useModelStatuses } from "@/store-model-statuses";
import { useUi } from "@/store-ui";
import { NATIVE_AGENT } from "@/constants";
import { StatusDot } from "@/components/common/bits";

type ModelPickerProps = {
  /** Raw runtime/model id ("free", "anthropic/claude-…") or a sentinel from `leading`. */
  value: string;
  onValueChange: (v: string) => void;
  /** Raw model ids; grouped by provider family via groupModelOptions. */
  models: string[];
  /** Sentinel options pinned ahead of the grouped list (e.g. "Router default…"). */
  leading?: ComboboxOption[];
  /** chip: ghost h-7 composer pill. field: default outline trigger, full width. */
  variant: "chip" | "field";
  placeholder?: string;
  ariaLabel: string;
  disabled?: boolean;
};

// The one model picker for the whole app: search always visible, popup wide
// enough for long mono ids, hide-invalid filtering wired to the persisted
// verdicts (the currently selected invalid model stays, warning-flagged).
export function ModelPicker({ value, onValueChange, models, leading, variant, placeholder, ariaLabel, disabled }: ModelPickerProps) {
  const catalog = useConnections((s) => s.catalog);
  const connections = useConnections((s) => s.connections);
  const byKey = useModelStatuses((s) => s.byKey);
  const hideInvalid = useUi((s) => s.hideInvalidModels);
  const nativeColor = NATIVE_AGENT.color;

  const options = useMemo(() => {
    const grouped = groupModelOptions(models, catalog, connections, {
      statuses: byKey,
      hideInvalid,
      selectedValue: value,
    });
    return (leading ?? []).reduceRight<ComboboxOption[] | ComboboxGroup[]>((acc, opt) => withLeadingOption(opt, acc), grouped);
  }, [models, catalog, connections, byKey, hideInvalid, value, leading]);

  return (
    <Combobox
      aria-label={ariaLabel}
      options={options}
      value={value}
      onValueChange={onValueChange}
      placeholder={placeholder}
      disabled={disabled}
      searchThreshold={0}
      popupClassName="min-w-[min(320px,var(--available-width))]"
      className={variant === "field" ? "min-w-0 flex-1" : undefined}
      trigger={
        variant === "chip" ? (
          <Button
            variant="ghost"
            size="sm"
            title={models.length === 0 ? "No models available. Add a provider connection in Models." : "Model"}
            className="max-w-[220px] font-semibold"
          >
            <StatusDot color={nativeColor} />
            <span className="min-w-0 truncate">{value || placeholder || "Default model"}</span>
            <ChevronDown aria-hidden size={11} strokeWidth={2} className="size-[11px] shrink-0" />
          </Button>
        ) : undefined
      }
    />
  );
}
