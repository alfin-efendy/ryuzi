// apps/ide/src/main/client.ts
import { createControlPlaneClient, type RemoteControlPlane } from "@harness/client";
import { EVENT_CHANNEL, CONNECTION_CHANNEL, APPROVAL_CHANNEL } from "../shared/ipc-contract";

export interface ClientHandle {
  client: RemoteControlPlane;
  connect(): Promise<void>;
  dispose(): void;
}

export function createSession(deps: {
  baseUrl: string;
  getToken: () => Promise<string>;
  send: (channel: string, payload: unknown) => void;
}): ClientHandle {
  const client = createControlPlaneClient({
    baseUrl: deps.baseUrl,
    getToken: deps.getToken,
  });
  const offEvent = client.onEvent((e) => deps.send(EVENT_CHANNEL, e));
  const offConn = client.onConnectionChange((s) => deps.send(CONNECTION_CHANNEL, s));
  const offApproval = client.onApprovalRequest((r) => deps.send(APPROVAL_CHANNEL, r));
  return {
    client,
    connect: () => client.connect(),
    dispose: () => {
      offEvent();
      offConn();
      offApproval();
      client.close();
    },
  };
}
