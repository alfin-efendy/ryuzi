import { useState } from "react";
import { describe, expect, test } from "bun:test";
import { fireEvent, render, screen } from "@testing-library/react";
import type { AgentSummaryInfo } from "@/bindings";
import { AgentMentionMenu } from "./AgentMentionMenu";

const agents: AgentSummaryInfo[] = [
  {
    id: "ada",
    name: "Ada",
    description: "",
    avatarColor: "blue",
    model: { kind: "route", route: "fast" },
    permissionMode: "ask",
    skillCount: 0,
    toolCount: 0,
    knowledgeCount: 0,
    executable: true,
    validation: [],
    isDefault: false,
  },
  {
    id: "lin",
    name: "Lin",
    description: "",
    avatarColor: "green",
    model: { kind: "route", route: "fast" },
    permissionMode: "ask",
    skillCount: 0,
    toolCount: 0,
    knowledgeCount: 0,
    executable: true,
    validation: [],
    isDefault: false,
  },
];

describe("AgentMentionMenu", () => {
  test("selects the active candidate with Enter and moves with ArrowDown", () => {
    const picked: string[] = [];
    function Menu() {
      const [activeIndex, setActiveIndex] = useState(0);
      return (
        <AgentMentionMenu
          agents={agents}
          activeIndex={activeIndex}
          onActiveIndexChange={setActiveIndex}
          onPick={(agent) => picked.push(agent.id)}
        />
      );
    }
    render(<Menu />);

    fireEvent.keyDown(screen.getByRole("menu"), { key: "ArrowDown" });
    fireEvent.keyDown(screen.getByRole("menu"), { key: "Enter" });

    expect(picked).toEqual(["lin"]);
  });
});
