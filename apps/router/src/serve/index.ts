// apps/router/src/serve/index.ts
import type { ServerWebSocket } from "bun";
import { RPC_METHODS, type RpcRequest, type RpcResponse, type ClientFrame, type ServerFrame, type RpcMethod } from "@harness/protocol";
import type { ControlPlane } from "../core/control-plane";
import type { SettingsStore } from "../config/store";
import { ConnectionHub } from "./connections";
import { RemoteGateway } from "./remote-gateway";
import { createAuthenticator, type Authenticator } from "./auth";

export interface ServeOptions {
  settings: SettingsStore;
  host?: string;
  port?: number;
  localToken: string;
}

interface SocketData {
  connId: string;
  actor: string;
}

// Methods whose params object carries an `actor` we must overwrite with the
// authenticated actor (never trust client-supplied actor).
const ACTOR_METHODS = new Set<RpcMethod>(["startSession", "continueSession", "connectProject"]);

async function dispatch(cp: ControlPlane, method: RpcMethod, params: any, actor: string): Promise<unknown> {
  const p = params ?? {};
  if (ACTOR_METHODS.has(method)) p.actor = actor;
  switch (method) {
    case "listProjects":
      return cp.listProjects();
    case "getProject":
      return cp.getProject(p.id);
    case "listSessions":
      return cp.listSessions(p.projectId);
    case "startSession":
      return cp.startSession(p);
    case "continueSession":
      return cp.continueSession(p);
    case "connectProject":
      return cp.connectProject(p);
    case "stopSession":
      return cp.stopSession(p.sessionPk);
    case "endSession":
      return cp.endSession(p.sessionPk, p.opts);
    case "listDir":
      return cp.listDir(p);
    case "readFile":
      return cp.readFile(p);
    default:
      throw new Error("unhandled method: " + method);
  }
}

export function startServeServer(cp: ControlPlane, opts: ServeOptions): { url: string; port: number; stop(): void } {
  const hub = new ConnectionHub();
  cp.gateways.register(new RemoteGateway(hub));
  const auth: Authenticator = createAuthenticator({ settings: opts.settings, localToken: opts.localToken });

  // Fan out every CoreEvent to all live sockets.
  const sockets = new Set<ServerWebSocket<SocketData>>();
  const unsubscribe = cp.subscribe((event) => {
    const frame: ServerFrame = { t: "event", event };
    const msg = JSON.stringify(frame);
    for (const ws of sockets) ws.send(msg);
  });

  const server = Bun.serve<SocketData>({
    port: opts.port ?? 0,
    hostname: opts.host ?? "127.0.0.1",
    async fetch(req, srv) {
      const url = new URL(req.url);

      if (url.pathname === "/rpc" && req.method === "POST") {
        const authed = await auth.authenticate(req.headers.get("authorization"));
        if (!authed) return new Response("unauthorized", { status: 401 });
        let body: RpcRequest;
        try {
          body = (await req.json()) as RpcRequest;
        } catch {
          return new Response("bad json", { status: 400 });
        }
        if (!RPC_METHODS.includes(body.method)) {
          return Response.json({ id: body?.id ?? "0", ok: false, error: `unknown method: ${body?.method}` } satisfies RpcResponse);
        }
        try {
          const result = await dispatch(cp, body.method, body.params, authed.actor);
          return Response.json({ id: body.id, ok: true, result } satisfies RpcResponse);
        } catch (e) {
          return Response.json({ id: body.id, ok: false, error: e instanceof Error ? e.message : String(e) } satisfies RpcResponse);
        }
      }

      if (url.pathname === "/ws-ticket" && req.method === "POST") {
        const authed = await auth.authenticate(req.headers.get("authorization"));
        if (!authed) return new Response("unauthorized", { status: 401 });
        return Response.json({ ticket: auth.issueTicket(authed.actor) });
      }

      if (url.pathname === "/ws" && req.method === "GET") {
        const ticket = url.searchParams.get("ticket");
        const authed = ticket ? auth.consumeTicket(ticket) : null;
        if (!authed) return new Response("unauthorized", { status: 401 });
        const connId = crypto.randomUUID();
        if (srv.upgrade(req, { data: { connId, actor: authed.actor } })) return undefined;
        return new Response("upgrade failed", { status: 400 });
      }

      return new Response("not found", { status: 404 });
    },
    websocket: {
      open(ws) {
        sockets.add(ws);
        hub.add(ws.data.connId, (f) => ws.send(JSON.stringify(f)));
        ws.send(JSON.stringify({ t: "hello", connId: ws.data.connId } satisfies ServerFrame));
      },
      message(ws, raw) {
        let frame: ClientFrame;
        try {
          frame = JSON.parse(String(raw)) as ClientFrame;
        } catch {
          return;
        }
        if (frame.t === "approval.resolve") hub.resolveApproval(frame.requestId, frame.decision);
        else if (frame.t === "ping") ws.send(JSON.stringify({ t: "pong" } satisfies ServerFrame));
      },
      close(ws) {
        sockets.delete(ws);
        hub.remove(ws.data.connId);
      },
    },
  });

  return {
    url: `http://${server.hostname}:${server.port}`,
    port: server.port!,
    stop: () => {
      unsubscribe();
      server.stop(true);
    },
  };
}
