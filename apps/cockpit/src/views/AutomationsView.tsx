import { useState } from "react";
import { Segmented, SettingsCard } from "@ryuzi/ui";
import { SchedulerView } from "./SchedulerView";

type AutomationTab = "scheduler" | "hooks" | "commands";

const TABS: { id: AutomationTab; label: string }[] = [
  { id: "scheduler", label: "Scheduler" },
  { id: "hooks", label: "Hooks" },
  { id: "commands", label: "Commands" },
];

function UnavailableTab({ name }: { name: string }) {
  return (
    <div className="min-h-0 flex-1 overflow-y-auto px-8 py-7">
      <div className="mx-auto max-w-[860px]">
        <SettingsCard className="p-6 text-center text-[13px] text-muted-foreground">{name} are not available yet.</SettingsCard>
      </div>
    </div>
  );
}

export function AutomationsView({ initialTab = "scheduler" }: { initialTab?: AutomationTab }) {
  const [tab, setTab] = useState<AutomationTab>(initialTab);

  return (
    <div className="flex min-h-0 flex-1 flex-col">
      <div className="shrink-0 border-b border-border px-8 py-3">
        <div className="mx-auto max-w-[860px]">
          <Segmented options={TABS} value={tab} onChange={setTab} />
        </div>
      </div>
      {tab === "scheduler" ? <SchedulerView /> : <UnavailableTab name={tab === "hooks" ? "Hooks" : "Commands"} />}
    </div>
  );
}
