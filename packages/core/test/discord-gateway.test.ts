// packages/core/test/discord-gateway.test.ts
import { test, expect } from "bun:test";
import { DiscordGateway, type DiscordPort, type InboundRouter, type InboundMessage } from "../src/gateways/discord/index";

class FakePort implements DiscordPort {
  calls: string[] = [];
  connected = false;
  private n = 0;
  lastApproval?: unknown;
  botUserId() {
    return "bot";
  }
  async connect(_h?: unknown) {
    this.connected = true;
  }
  async createTextChannel(name: string) {
    this.calls.push(`createTextChannel:${name}`);
    return `chan-${++this.n}`;
  }
  async createThread(channelId: string, name: string) {
    this.calls.push(`createThread:${channelId}:${name}`);
    return `thread-${++this.n}`;
  }
  async sendMessage(channelId: string, text: string) {
    this.calls.push(`send:${channelId}:${text}`);
    return `msg-${++this.n}`;
  }
  async editMessage(channelId: string, messageId: string, text: string) {
    this.calls.push(`edit:${channelId}:${messageId}:${text}`);
  }
  async requestApproval(conversationId: string, req: unknown) {
    this.calls.push(`requestApproval:${conversationId}`);
    this.lastApproval = req;
    return { decision: "deny" as const, actor: "u9" };
  }
}

function fakeRouter() {
  const calls: string[] = [];
  const attachmentsSeen: number[] = [];
  const router: InboundRouter = {
    onConnect: async (_g, _a, o) => {
      calls.push(`onConnect:${o.name ?? o.gitUrl}`);
      return { workspaceId: "ws-1", project: { name: o.name ?? "p" } };
    },
    onStart: async (_g, w, _a, p, atts) => {
      calls.push(`onStart:${w}:${p}`);
      attachmentsSeen.push(atts?.length ?? 0);
    },
    onReply: async (_g, c, _a, p, atts) => {
      calls.push(`onReply:${c}:${p}`);
      attachmentsSeen.push(atts?.length ?? 0);
    },
    onEnd: async (_g, c) => {
      calls.push(`onEnd:${c}`);
    },
    onStop: async (_g, c) => {
      calls.push(`onStop:${c}`);
    },
  };
  return { router, calls, attachmentsSeen };
}

const msg = (over: Partial<InboundMessage>): InboundMessage => ({
  channelId: "c",
  isThread: false,
  authorBot: false,
  authorId: "u",
  mentionsBot: false,
  content: "",
  attachments: [],
  ...over,
});

test("output methods delegate to the port", async () => {
  const port = new FakePort();
  const { router } = fakeRouter();
  const gw = new DiscordGateway(port, router);
  const ws = await gw.createWorkspace("foo");
  const conv = await gw.createConversation(ws, "title");
  const ref = await gw.postStatus({ gateway: "discord", conversationId: conv }, "working");
  await gw.editStatus(ref, "done");
  await gw.postResult({ gateway: "discord", conversationId: conv }, ["a", "b"]);
  expect(port.calls).toEqual([
    "createTextChannel:foo",
    "createThread:chan-1:title",
    "send:thread-2:working",
    "edit:thread-2:msg-3:done",
    "send:thread-2:a",
    "send:thread-2:b",
  ]);
});

test("ignores bot messages; thread→reply; mention→start; else ignore", async () => {
  const port = new FakePort();
  const { router, calls } = fakeRouter();
  const gw = new DiscordGateway(port, router);
  await gw.handleMessage(msg({ authorBot: true, isThread: true, content: "x" })); // ignored
  await gw.handleMessage(msg({ isThread: true, channelId: "t1", content: "more" })); // reply
  await gw.handleMessage(msg({ mentionsBot: true, channelId: "ch1", content: "<@12345> do it" })); // start (mention stripped)
  await gw.handleMessage(msg({ content: "just chatting" })); // ignored
  expect(calls).toEqual(["onReply:t1:more", "onStart:ch1:do it"]);
});

test("interaction connect routes to onConnect and replies with the channel", async () => {
  const port = new FakePort();
  const { router, calls } = fakeRouter();
  const gw = new DiscordGateway(port, router);
  const replies: string[] = [];
  await gw.handleInteraction({ name: "connect", userId: "u", channelId: "c", options: { name: "foo" } }, async (t) => {
    replies.push(t);
  });
  expect(calls).toEqual(["onConnect:foo"]);
  expect(replies[0]).toContain("ws-1");
});

test("start() connects the port", async () => {
  const port = new FakePort();
  const { router } = fakeRouter();
  const gw = new DiscordGateway(port, router);
  await gw.start();
  expect(port.connected).toBe(true);
});

test("requestApproval forwards to the port and returns its decision", async () => {
  const port = new FakePort();
  const { router } = fakeRouter();
  const gw = new DiscordGateway(port, router);
  const dec = await gw.requestApproval(
    { gateway: "discord", conversationId: "t1" },
    { requestId: "r1", tool: "Bash", summary: "Bash: rm", approverRoleIds: ["r1"], startedBy: "u1", timeoutMs: 1000 },
  );
  expect(dec).toEqual({ decision: "deny", actor: "u9" });
  expect(port.calls).toContain("requestApproval:t1");
  expect((port.lastApproval as { approverRoleIds: string[] }).approverRoleIds).toEqual(["r1"]);
});

test("attachment-only messages start/reply even with empty text", async () => {
  const port = new FakePort();
  const { router, calls, attachmentsSeen } = fakeRouter();
  const gw = new DiscordGateway(port, router);
  const att = { name: "a.png", url: "https://cdn/a", contentType: "image/png", size: 10 };
  await gw.handleMessage(msg({ mentionsBot: true, channelId: "ch1", content: "<@1>", attachments: [att] })); // start, empty prompt
  await gw.handleMessage(msg({ isThread: true, channelId: "t1", content: "", attachments: [att] })); // reply
  await gw.handleMessage(msg({ mentionsBot: true, channelId: "ch2", content: "<@1>" })); // ignored: no text, no attachment
  expect(calls).toEqual(["onStart:ch1:", "onReply:t1:"]);
  expect(attachmentsSeen).toEqual([1, 1]);
});
