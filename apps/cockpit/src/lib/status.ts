import type { Session } from "../bindings";

export type StatusMeta = { label: string; color: string; pulse: boolean };

// Real session statuses projected onto the design's status language.
export function statusMeta(status: Session["status"]): StatusMeta {
  switch (status) {
    case "running":
      return { label: "Running", color: "#3B82F6", pulse: true };
    case "idle":
      return { label: "Waiting", color: "#F59E0B", pulse: false };
    case "interrupted":
      return { label: "Stopped", color: "#9A9A9A", pulse: false };
    case "ended":
      return { label: "Done", color: "#22C55E", pulse: false };
  }
}
