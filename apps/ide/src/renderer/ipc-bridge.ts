// apps/ide/src/renderer/ipc-bridge.ts
import { useStore } from "./store";

export function hydrate(): () => void {
  const { setConnection, setConnId, setProjects, setSessions, applyEvent, addApproval, clearApprovals } = useStore.getState();

  async function snapshot() {
    setConnId(await window.harness.getConnId());
    setProjects(await window.harness.listProjects());
    setSessions(await window.harness.listSessions());
  }

  const offEvent = window.harness.onEvent((e) => applyEvent(e));
  const offConn = window.harness.onConnectionChange((s) => {
    setConnection(s);
    if (s === "open") void snapshot();
    if (s === "closed") clearApprovals();
  });
  const offApproval = window.harness.onApprovalRequest((r) => addApproval(r));
  return () => {
    offEvent();
    offConn();
    offApproval();
  };
}
