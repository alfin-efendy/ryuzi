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
  Unsubscribe,
  CoreEvent,
  ServerFrame,
  ClientFrame,
  ApprovalRequestFrame,
  WsTicketResponse,
} from "@harness/protocol";

export type ConnState = "connecting" | "open" | "closed";

export interface ClientOptions {
  baseUrl: string;
  getToken: () => Promise<string>;
  fetchImpl?: typeof fetch;
  /** Injected WebSocket constructor for tests. Defaults to global WebSocket. */
  WebSocketImpl?: typeof WebSocket;
  autoReconnect?: boolean;
  reconnectBaseMs?: number;
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
  connId: string | null;
  connect(): Promise<void>;
  close(): void;
  onEvent(cb: (e: CoreEvent) => void): Unsubscribe;
  onApprovalRequest(cb: (r: ApprovalRequestFrame) => void): Unsubscribe;
  resolveApproval(requestId: string, decision: "allow" | "deny"): void;
  onConnectionChange(cb: (s: ConnState) => void): Unsubscribe;
}

let counter = 0;
function nextId(): string {
  counter += 1;
  return String(counter);
}

export function createControlPlaneClient(opts: ClientOptions): RemoteControlPlane {
  const fetchImpl = opts.fetchImpl ?? fetch;
  const base = opts.baseUrl.replace(/\/$/, "");
  const WS = opts.WebSocketImpl ?? WebSocket;
  const autoReconnect = opts.autoReconnect ?? true;
  const baseMs = opts.reconnectBaseMs ?? 500;
  const wsUrl = base.replace(/^http/, "ws");

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

  const eventCbs = new Set<(e: CoreEvent) => void>();
  const approvalCbs = new Set<(r: ApprovalRequestFrame) => void>();
  const stateCbs = new Set<(s: ConnState) => void>();
  let ws: WebSocket | null = null;
  let connId: string | null = null;
  let closedByUser = false;
  let attempt = 0;

  function on<T>(set: Set<T>, cb: T): Unsubscribe {
    set.add(cb);
    return () => set.delete(cb);
  }
  function setState(s: ConnState) {
    for (const cb of stateCbs) cb(s);
  }
  function send(frame: ClientFrame) {
    if (ws && ws.readyState === 1) ws.send(JSON.stringify(frame)); // 1 === WebSocket.OPEN (WHATWG constant)
  }

  async function openSocket(): Promise<void> {
    setState("connecting");
    const token = await opts.getToken();
    const res = await fetchImpl(`${base}/ws-ticket`, { method: "POST", headers: { authorization: `Bearer ${token}` } });
    if (!res.ok) throw new Error(`ws-ticket failed: HTTP ${res.status}`);
    const { ticket } = (await res.json()) as WsTicketResponse;
    const sock = new WS(`${wsUrl}/ws?ticket=${encodeURIComponent(ticket)}`);
    ws = sock;
    sock.onopen = () => {
      attempt = 0;
      setState("open");
    };
    sock.onmessage = (e: { data: unknown }) => {
      const frame = JSON.parse(String(e.data)) as ServerFrame;
      if (frame.t === "hello") connId = frame.connId;
      else if (frame.t === "event") for (const cb of eventCbs) cb(frame.event);
      else if (frame.t === "approval.request") for (const cb of approvalCbs) cb(frame);
    };
    sock.onclose = () => {
      ws = null;
      connId = null;
      setState("closed");
      if (!closedByUser && autoReconnect) {
        attempt += 1;
        const delay = Math.min(baseMs * 2 ** (attempt - 1), 10_000) + Math.floor(Math.random() * baseMs);
        setTimeout(() => void openSocket().catch(() => {}), delay);
      }
    };
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
    get connId() {
      return connId;
    },
    connect: () => {
      if (ws && ws.readyState < 2) return Promise.resolve();
      closedByUser = false;
      attempt = 0;
      return openSocket();
    },
    close: () => {
      closedByUser = true;
      ws?.close();
    },
    onEvent: (cb) => on(eventCbs, cb),
    onApprovalRequest: (cb) => on(approvalCbs, cb),
    onConnectionChange: (cb) => on(stateCbs, cb),
    resolveApproval: (requestId, decision) => send({ t: "approval.resolve", requestId, decision }),
  };
}
