import type { ReactNode } from "react";
import { ArrowLeft } from "lucide-react";
import { Button } from "@ryuzi/ui";

// Detail-screen header: breadcrumb back button, identity chip, title/subtitle,
// then any right-aligned actions.
export function BackButton({ label, onClick }: { label: string; onClick: () => void }) {
  return (
    <Button variant="ghost" size="sm" onClick={onClick} className="-ml-1.5 mb-3.5 flex gap-1.5 pl-1.5 pr-2.5 text-muted-foreground">
      <ArrowLeft aria-hidden size={13} strokeWidth={2} className="size-[13px]" />
      {label}
    </Button>
  );
}

export function DetailHeader({
  chip,
  title,
  titleNode,
  titleExtra,
  sub,
  children,
}: {
  chip: ReactNode;
  title: string;
  titleNode?: ReactNode;
  titleExtra?: ReactNode;
  sub: string;
  children?: ReactNode;
}) {
  return (
    <div className="mb-5 flex items-center gap-3.5">
      {chip}
      <div className="min-w-0 flex-1">
        <div className="flex items-center gap-2">
          {titleNode ?? (
            <span className="overflow-hidden text-ellipsis whitespace-nowrap text-xl font-semibold tracking-[-0.02em]">{title}</span>
          )}
          {titleExtra}
        </div>
        <div className="text-[12.5px] text-muted-foreground">{sub}</div>
      </div>
      {children}
    </div>
  );
}
