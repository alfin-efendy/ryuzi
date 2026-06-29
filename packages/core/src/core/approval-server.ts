// packages/core/src/core/approval-server.ts
export interface ApproveBody {
  sessionPk: string;
  tool: string;
  input: unknown;
}
export interface Approver {
  requestApproval(req: ApproveBody): Promise<"allow" | "deny">;
}

export async function handleApprove(body: ApproveBody, approver: Approver): Promise<{ permissionDecision: "allow" | "deny" }> {
  const decision = await approver.requestApproval(body);
  return { permissionDecision: decision };
}

export function startApprovalServer(approver: Approver): { url: string; stop(): void } {
  const token = crypto.randomUUID();
  const server = Bun.serve({
    port: 0,
    hostname: "127.0.0.1",
    async fetch(req) {
      if (req.method !== "POST") return new Response("method not allowed", { status: 405 });
      if (new URL(req.url).pathname !== `/${token}`) return new Response("forbidden", { status: 403 });
      let body: ApproveBody;
      try {
        body = (await req.json()) as ApproveBody;
      } catch {
        return new Response("bad json", { status: 400 });
      }
      return Response.json(await handleApprove(body, approver));
    },
  });
  return { url: `http://127.0.0.1:${server.port}/${token}`, stop: () => server.stop(true) };
}
