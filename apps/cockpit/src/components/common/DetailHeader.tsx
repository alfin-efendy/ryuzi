import type { ReactNode } from "react";
import { ArrowLeft } from "lucide-react";

// Detail-screen header: breadcrumb back button, identity chip, title/subtitle,
// then any right-aligned actions.
export function BackButton({ label, onClick }: { label: string; onClick: () => void }) {
  return (
    <button
      type="button"
      onClick={onClick}
      className="-ml-1.5 mb-3.5 flex h-7 cursor-pointer items-center gap-1.5 rounded-md border-none bg-transparent pl-1.5 pr-2.5 font-sans text-[12.5px] font-medium text-muted-foreground hover:bg-accent hover:text-accent-foreground"
    >
      <ArrowLeft aria-hidden size={13} strokeWidth={2} />
      {label}
    </button>
  );
}

export function DetailHeader({
  chip,
  title,
  titleExtra,
  sub,
  children,
}: {
  chip: ReactNode;
  title: string;
  titleExtra?: ReactNode;
  sub: string;
  children?: ReactNode;
}) {
  return (
    <div className="mb-5 flex items-center gap-3.5">
      {chip}
      <div className="min-w-0 flex-1">
        <div className="flex items-center gap-2">
          <span className="overflow-hidden text-ellipsis whitespace-nowrap text-xl font-semibold tracking-[-0.02em]">{title}</span>
          {titleExtra}
        </div>
        <div className="text-[12.5px] text-muted-foreground">{sub}</div>
      </div>
      {children}
    </div>
  );
}
