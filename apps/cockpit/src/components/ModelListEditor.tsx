import { Plus, TestTube2, X } from "lucide-react";
import { useState } from "react";
import { Button, Input } from "@ryuzi/ui";
import { ModelCapabilityIcons } from "@/components/ModelCapabilityIcons";

function normalizeModel(value: string) {
  return value.trim();
}

export function ModelListEditor({
  models,
  testingModel,
  onChange,
  onTestModel,
}: {
  models: string[];
  testingModel: string | null;
  onChange: (models: string[]) => void;
  onTestModel: (model: string) => void;
}) {
  const [draft, setDraft] = useState("");

  const add = () => {
    const model = normalizeModel(draft);
    if (!model || models.includes(model)) return;
    onChange([...models, model]);
    setDraft("");
  };

  const remove = (model: string) => {
    onChange(models.filter((item) => item !== model));
  };

  return (
    <div className="px-[18px] py-3">
      <div className="flex items-center gap-2">
        <Input
          className="flex-1 font-mono text-xs"
          value={draft}
          onChange={(event) => setDraft(event.target.value)}
          onKeyDown={(event) => {
            if (event.key === "Enter") {
              event.preventDefault();
              add();
            }
          }}
          placeholder="Add model id"
        />
        <Button variant="outline" onClick={add} disabled={!normalizeModel(draft)}>
          <Plus aria-hidden size={13} strokeWidth={2} className="size-[13px]" />
          Add model
        </Button>
      </div>

      <div className="mt-3 overflow-hidden rounded-lg border border-border">
        {models.map((model) => (
          <div key={model} className="flex min-h-11 items-center gap-2 border-b border-border px-3 py-2 last:border-b-0">
            <span className="min-w-0 flex-1 truncate font-mono text-xs text-foreground">{model}</span>
            <ModelCapabilityIcons model={model} compact />
            <Button
              variant="outline"
              size="sm"
              onClick={() => onTestModel(model)}
              disabled={testingModel === model}
              aria-label={`Test ${model}`}
            >
              <TestTube2 aria-hidden size={12} strokeWidth={2} className="size-3" />
              {testingModel === model ? "Testing..." : "Test"}
            </Button>
            <Button
              variant="ghost"
              size="icon-sm"
              title={`Remove ${model}`}
              aria-label={`Remove ${model}`}
              onClick={() => remove(model)}
              className="text-muted-foreground"
            >
              <X aria-hidden size={13} strokeWidth={2} className="size-[13px]" />
            </Button>
          </div>
        ))}
        {models.length === 0 && <div className="px-3 py-3 text-[12.5px] text-muted-foreground">Using provider default models.</div>}
      </div>

      <div className="mt-1.5 text-[11.5px] text-muted-foreground">Empty list falls back to the provider's default list.</div>
    </div>
  );
}
