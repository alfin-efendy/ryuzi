import { CircleAlert, OctagonAlert } from "lucide-react";
import { useEffect } from "react";
import { Button, Modal, ModalFooter } from "@ryuzi/ui";
import type { DoctorFinding } from "@/bindings";
import { usePlugins } from "@/store-plugins";

const WARN = "#F59E0B";
const DANGER = "#EF4444";

function FindingRow({ finding, color, Icon }: { finding: DoctorFinding; color: string; Icon: typeof CircleAlert }) {
  return (
    <div className="flex items-start gap-2.5 rounded-md border px-3 py-2.5 text-[12.5px]" style={{ borderColor: color }}>
      <Icon aria-hidden size={14} strokeWidth={2} className="mt-0.5 shrink-0" style={{ color }} />
      <div className="min-w-0 flex-1">
        <div className="font-medium" style={{ color }}>
          {finding.pluginId}
        </div>
        <div className="mt-0.5 text-muted-foreground">{finding.message}</div>
        <div className="mt-1 text-[11.5px] text-muted-foreground">{finding.suggestedAction}</div>
      </div>
    </div>
  );
}

// Read-only plugin health aggregation (see `ryuzi_core::plugins::doctor`) —
// grouped by severity so errors (missing binaries) surface above warnings
// (reconnect-required, attach-failed). Opened as an overlay from the Plugins
// hub's doctor summary chip; re-fetches every time it's opened rather than
// trusting a stale cached list, since it exists specifically to answer
// "what's wrong right now."
export function DoctorPanel({ onClose }: { onClose: () => void }) {
  const findings = usePlugins((s) => s.doctorFindings);
  const loadDoctor = usePlugins((s) => s.loadDoctor);

  useEffect(() => {
    void loadDoctor();
  }, [loadDoctor]);

  const errors = findings.filter((f) => f.severity === "error");
  const warns = findings.filter((f) => f.severity !== "error");

  return (
    <Modal onClose={onClose} width={480}>
      <div className="mb-3 text-[15px] font-semibold tracking-[-0.01em]">Plugin doctor</div>
      {findings.length === 0 ? (
        <p className="m-0 py-6 text-center text-[12.5px] text-muted-foreground">No issues found.</p>
      ) : (
        <div className="flex flex-col gap-2">
          {errors.map((f, i) => (
            <FindingRow key={`error-${f.pluginId}-${f.kind}-${i}`} finding={f} color={DANGER} Icon={OctagonAlert} />
          ))}
          {warns.map((f, i) => (
            <FindingRow key={`warn-${f.pluginId}-${f.kind}-${i}`} finding={f} color={WARN} Icon={CircleAlert} />
          ))}
        </div>
      )}
      <ModalFooter>
        <Button onClick={onClose}>Close</Button>
      </ModalFooter>
    </Modal>
  );
}
