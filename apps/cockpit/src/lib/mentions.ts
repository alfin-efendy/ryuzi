import type { AgentMention } from "@/bindings";

export type MentionDraft = {
  text: string;
  mentions: AgentMention[];
};

export type MentionAgent = {
  id: string;
  name: string;
  description: string;
  executable: boolean;
};

export type AgentMentionQuery = {
  start: number;
  end: number;
  query: string;
};

function isTokenBoundary(text: string, index: number): boolean {
  return index === 0 || /\s/.test(text[index - 1] ?? "");
}

export function activeAgentMentionQuery(text: string, caret: number): AgentMentionQuery | null {
  const beforeCaret = text.slice(0, caret);
  const match = /(^|\s)@([^\s@]*)$/.exec(beforeCaret);
  if (!match) return null;
  const start = match.index + match[1].length;
  if (!isTokenBoundary(text, start)) return null;
  return { start, end: caret, query: match[2] };
}

export function matchMentionAgents<T extends MentionAgent>(
  agents: T[],
  query: string,
  primaryAgentId: string | null,
  _mentions: AgentMention[],
): T[] {
  const normalizedQuery = query.toLocaleLowerCase();
  return agents.filter(
    (agent) =>
      agent.executable &&
      agent.id !== primaryAgentId &&
      (agent.name.toLocaleLowerCase().includes(normalizedQuery) || agent.description.toLocaleLowerCase().includes(normalizedQuery)),
  );
}

export function updateMentionDraft(previous: MentionDraft, text: string): MentionDraft {
  let changeStart = 0;
  while (changeStart < previous.text.length && changeStart < text.length && previous.text[changeStart] === text[changeStart]) {
    changeStart += 1;
  }

  let previousEnd = previous.text.length;
  let textEnd = text.length;
  while (previousEnd > changeStart && textEnd > changeStart && previous.text[previousEnd - 1] === text[textEnd - 1]) {
    previousEnd -= 1;
    textEnd -= 1;
  }

  const delta = textEnd - previousEnd;
  const mentions = previous.mentions.flatMap((mention) => {
    if (previous.text.slice(mention.startUtf16, mention.endUtf16) !== `@${mention.labelSnapshot}`) return [];
    if (mention.endUtf16 <= changeStart) return [mention];
    if (mention.startUtf16 >= previousEnd) {
      return [{ ...mention, startUtf16: mention.startUtf16 + delta, endUtf16: mention.endUtf16 + delta }];
    }
    return [];
  });
  return { text, mentions };
}

export function insertAgentMention(draft: MentionDraft, caret: number, agent: MentionAgent): MentionDraft {
  const active = activeAgentMentionQuery(draft.text, caret);
  if (!active) return draft;

  const token = `@${agent.name}`;
  const text = `${draft.text.slice(0, active.start)}${token} ${draft.text.slice(active.end)}`;
  const insertedLength = token.length + 1;
  const delta = insertedLength - (active.end - active.start);
  const mentions = [
    ...draft.mentions.map((mention) =>
      mention.startUtf16 >= active.end
        ? { ...mention, startUtf16: mention.startUtf16 + delta, endUtf16: mention.endUtf16 + delta }
        : mention,
    ),
    { agentId: agent.id, labelSnapshot: agent.name, startUtf16: active.start, endUtf16: active.start + token.length },
  ].sort((a, b) => a.startUtf16 - b.startUtf16);

  return { text, mentions };
}
