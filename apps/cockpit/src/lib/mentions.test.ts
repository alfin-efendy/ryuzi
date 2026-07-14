import { describe, expect, test } from "bun:test";
import type { AgentMention } from "@/bindings";
import {
  activeAgentMentionQuery,
  insertAgentMention,
  matchMentionAgents,
  updateMentionDraft,
  type MentionDraft,
} from "./mentions";

const ada = { id: "ada", name: "Ada", executable: true };
const lin = { id: "lin", name: "Lin", executable: true };
const blocked = { id: "blocked", name: "Blocked", executable: false };

function draft(text: string, mentions: AgentMention[] = []): MentionDraft {
  return { text, mentions };
}

describe("insertAgentMention", () => {
  test("replaces the active query and records UTF-16 DOM offsets after emoji", () => {
    const result = insertAgentMention(draft("😀 ask @ad"), 10, ada);

    expect(result).toEqual({
      text: "😀 ask @Ada ",
      mentions: [{ agentId: "ada", labelSnapshot: "Ada", startUtf16: 7, endUtf16: 11 }],
    });
  });

  test("does not add the same agent twice", () => {
    const existing = draft("@Ada and @a", [{ agentId: "ada", labelSnapshot: "Ada", startUtf16: 0, endUtf16: 4 }]);

    expect(insertAgentMention(existing, existing.text.length, ada)).toEqual(existing);
  });
});

describe("updateMentionDraft", () => {
  const mention: AgentMention = { agentId: "ada", labelSnapshot: "Ada", startUtf16: 7, endUtf16: 11 };

  test("shifts a mention by UTF-16 code units when text is inserted before it", () => {
    expect(updateMentionDraft(draft("😀 ask @Ada", [mention]), "😀 please ask @Ada")).toEqual(
      draft("😀 please ask @Ada", [{ ...mention, startUtf16: 14, endUtf16: 18 }]),
    );
  });

  test("keeps a mention when text is pasted after it", () => {
    expect(updateMentionDraft(draft("ask @Ada", [{ ...mention, startUtf16: 4, endUtf16: 8 }]), "ask @Ada then review")).toEqual(
      draft("ask @Ada then review", [{ ...mention, startUtf16: 4, endUtf16: 8 }]),
    );
  });

  test("removes a mention when an edit overlaps its token", () => {
    expect(updateMentionDraft(draft("ask @Ada", [{ ...mention, startUtf16: 4, endUtf16: 8 }]), "ask @Ava")).toEqual(draft("ask @Ava"));
  });

  test("removes a mention when deletion covers its token", () => {
    expect(updateMentionDraft(draft("ask @Ada now", [{ ...mention, startUtf16: 4, endUtf16: 8 }]), "ask  now")).toEqual(draft("ask  now"));
  });
});

describe("agent mention query and candidates", () => {
  test("finds an @ query at the caret and not an email address", () => {
    expect(activeAgentMentionQuery("ask @ad", 7)).toEqual({ start: 4, end: 7, query: "ad" });
    expect(activeAgentMentionQuery("email me@ada", 12)).toBeNull();
  });

  test("matches names case-insensitively while excluding the primary, non-executable, and mentioned agents", () => {
    const mentioned: AgentMention[] = [{ agentId: "ada", labelSnapshot: "Ada", startUtf16: 0, endUtf16: 4 }];

    expect(matchMentionAgents([ada, lin, blocked], "LI", "ada", mentioned)).toEqual([lin]);
    expect(matchMentionAgents([ada, lin, blocked], "", "lin", mentioned)).toEqual([]);
  });
});
