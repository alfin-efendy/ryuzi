import { CircleAlert, Pencil, RefreshCw, Wand2 } from "lucide-react";
import { SettingsCard, SettingsCardHeader, SettingsCardTitle } from "@ryuzi/ui";
import type { CuratorRun, SkillUsage } from "@/bindings";
import { buildActivityFeed, formatRelativeTime } from "@/store-learning";

// Self-improvement activity feed (Task-12 resolution #1): Task 11 exposes no
// command that lists the "💾 Self-improvement review" transcript notices, so
// this composes the same story — what the learning loop did — from data the
// frozen API does provide: skill_usage rows the agent authored/patched, and
// curator sweep history. The literal 💾 notices still show inline in each
// session's transcript; this panel doesn't duplicate them.
export function ReviewFeed({ skills, curatorRuns }: { skills: SkillUsage[]; curatorRuns: CuratorRun[] }) {
  const feed = buildActivityFeed(skills, curatorRuns);

  return (
    <SettingsCard>
      <SettingsCardHeader>
        <SettingsCardTitle>Activity</SettingsCardTitle>
        <span className="ml-auto text-xs text-muted-foreground">{feed.length}</span>
      </SettingsCardHeader>
      <div className="max-h-[280px] overflow-y-auto">
        {feed.length === 0 ? (
          <div className="px-[18px] py-6 text-center text-[12.5px] text-muted-foreground">No self-improvement activity yet.</div>
        ) : (
          feed.map((item) =>
            item.kind === "skill" ? (
              <SkillActivityRow key={`skill:${item.skill.name}:${item.at}`} skill={item.skill} at={item.at} />
            ) : (
              <CuratorActivityRow key={`curator:${item.run.id}`} run={item.run} at={item.at} />
            ),
          )
        )}
      </div>
    </SettingsCard>
  );
}

function SkillActivityRow({ skill, at }: { skill: SkillUsage; at: number }) {
  const created = skill.createdBy === "agent" && skill.patchCount === 0;
  const Icon = created ? Wand2 : Pencil;
  const what = created ? "created by the agent" : `patched ${skill.patchCount} time${skill.patchCount === 1 ? "" : "s"}`;
  return (
    <div className="flex items-center gap-2.5 border-b border-border px-[18px] py-2.5 text-[12.5px] last:border-b-0">
      <Icon aria-hidden size={14} strokeWidth={2} className="shrink-0 text-muted-foreground" />
      <span className="min-w-0 flex-1 truncate">
        <span className="font-mono font-medium">{skill.name}</span> <span className="text-muted-foreground">{what}</span>
      </span>
      <span className="shrink-0 text-[11px] text-muted-foreground">{formatRelativeTime(at)}</span>
    </div>
  );
}

function CuratorActivityRow({ run, at }: { run: CuratorRun; at: number }) {
  return (
    <div className="flex items-center gap-2.5 border-b border-border px-[18px] py-2.5 text-[12.5px] last:border-b-0">
      {run.status === "error" ? (
        <CircleAlert aria-hidden size={14} strokeWidth={2} className="shrink-0 text-destructive" />
      ) : (
        <RefreshCw aria-hidden size={14} strokeWidth={2} className="shrink-0 text-muted-foreground" />
      )}
      <span className="min-w-0 flex-1 truncate">
        <span className="font-medium">Curator sweep</span>{" "}
        <span className="text-muted-foreground">
          — {run.status}
          {run.status === "ok" ? `, ${run.transitioned} transitioned` : ""}
        </span>
      </span>
      <span className="shrink-0 text-[11px] text-muted-foreground">{formatRelativeTime(at)}</span>
    </div>
  );
}
