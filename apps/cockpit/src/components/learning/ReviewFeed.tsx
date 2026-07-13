import { MessageSquareText } from "lucide-react";
import { SettingsCard, SettingsCardRow } from "@ryuzi/ui";
import type { LearningReviewInfo } from "@/bindings";

export function ReviewFeed({ reviews }: { reviews: LearningReviewInfo[] }) {
  const ordered = [...reviews].sort((a, b) => b.timestamp.localeCompare(a.timestamp));
  return (
    <SettingsCard>
      {ordered.length === 0 ? (
        <div className="px-[18px] py-6 text-center text-xs text-muted-foreground">No learning reviews yet.</div>
      ) : (
        ordered.map((review) => (
          <SettingsCardRow key={`${review.conceptId}:${review.timestamp}`} className="items-start">
            <MessageSquareText aria-hidden size={14} className="mt-0.5 shrink-0 text-muted-foreground" />
            <div className="min-w-0 flex-1">
              <div className="text-xs font-medium">{review.title}</div>
              <p className="mb-0 mt-1 text-[11px] text-muted-foreground">{review.description}</p>
            </div>
            <time className="shrink-0 text-[11px] text-muted-foreground">{new Date(review.timestamp).toLocaleDateString()}</time>
          </SettingsCardRow>
        ))
      )}
    </SettingsCard>
  );
}
