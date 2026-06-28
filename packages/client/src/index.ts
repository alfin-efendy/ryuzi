// @harness/client — runtime-free client implementing ControlPlaneApi over HTTP+WS.
// Uses only global fetch + WebSocket (or injected impls for tests).
import type {
  Project,
  Session,
  StartSessionRequest,
  ContinueSessionRequest,
  ConnectProjectRequest,
  RpcMethod,
  RpcRequest,
  RpcResponse,
} from "@harness/protocol";

export interface ClientOptions {
  baseUrl: string;
  getToken: () => Promise<string>;
  fetchImpl?: typeof fetch;
  /** Injected WebSocket constructor for tests. Defaults to global WebSocket. */
  WebSocketImpl?: typeof WebSocket;
}

export interface RemoteControlPlane {
  listProjects(): Promise<Project[]>;
  getProject(id: string): Promise<Project | undefined>;
  listSessions(projectId?: string): Promise<Session[]>;
  startSession(req: StartSessionRequest): Promise<Session>;
  continueSession(req: ContinueSessionRequest): Promise<void>;
  connectProject(req: ConnectProjectRequest): Promise<Project>;
  stopSession(sessionPk: string): Promise<void>;
  endSession(sessionPk: string, opts?: { keepBranch?: boolean }): Promise<void>;
}

let counter = 0;
function nextId(): string {
  counter += 1;
  return String(counter);
}

export function createControlPlaneClient(opts: ClientOptions): RemoteControlPlane {
  const fetchImpl = opts.fetchImpl ?? fetch;
  const base = opts.baseUrl.replace(/\/$/, "");

  async function rpc<T>(method: RpcMethod, params?: unknown): Promise<T> {
    const token = await opts.getToken();
    const req: RpcRequest = { id: nextId(), method, params };
    const res = await fetchImpl(`${base}/rpc`, {
      method: "POST",
      headers: { "content-type": "application/json", authorization: `Bearer ${token}` },
      body: JSON.stringify(req),
    });
    if (!res.ok) throw new Error(`rpc ${method} failed: HTTP ${res.status}`);
    const data = (await res.json()) as RpcResponse;
    if (!data.ok) throw new Error(data.error);
    return data.result as T;
  }

  return {
    listProjects: () => rpc("listProjects"),
    getProject: (id) => rpc("getProject", { id }),
    listSessions: (projectId) => rpc("listSessions", { projectId }),
    startSession: (req) => rpc("startSession", req),
    continueSession: (req) => rpc("continueSession", req).then(() => undefined),
    connectProject: (req) => rpc("connectProject", req),
    stopSession: (sessionPk) => rpc("stopSession", { sessionPk }).then(() => undefined),
    endSession: (sessionPk, options) => rpc("endSession", { sessionPk, opts: options }).then(() => undefined),
  };
}
