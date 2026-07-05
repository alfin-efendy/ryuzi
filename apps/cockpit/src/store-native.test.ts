import { test, expect, spyOn } from "bun:test";
import { useNative } from "./store-native";
import { commands } from "./bindings";

function reset() {
  useNative.setState({ agentsByProject: {}, commandsByProject: {}, todosBySession: {} });
}

test("loadAgents caches the project's agents", async () => {
  reset();
  const spy = spyOn(commands, "nativeAgents").mockResolvedValue({
    status: "ok",
    data: [
      { name: "build", description: "Full access", mode: "primary", builtin: true },
      { name: "explore", description: "Read-only", mode: "subagent", builtin: true },
    ],
  });
  await useNative.getState().loadAgents("p1");
  expect(spy).toHaveBeenCalledWith("p1");
  expect(useNative.getState().agentsByProject.p1.map((a) => a.name)).toEqual(["build", "explore"]);
  spy.mockRestore();
});

test("loadTodos caches a session's todo list", async () => {
  reset();
  const spy = spyOn(commands, "sessionTodos").mockResolvedValue({
    status: "ok",
    data: [
      { content: "step one", status: "completed" },
      { content: "step two", status: "in_progress" },
    ],
  });
  await useNative.getState().loadTodos("s1");
  expect(spy).toHaveBeenCalledWith("s1");
  const todos = useNative.getState().todosBySession.s1;
  expect(todos).toHaveLength(2);
  expect(todos[1]).toEqual({ content: "step two", status: "in_progress" });
  spy.mockRestore();
});

test("a failed command leaves the cache untouched", async () => {
  reset();
  const spy = spyOn(commands, "nativeCommands").mockResolvedValue({
    status: "error",
    error: { message: "boom" },
  });
  await useNative.getState().loadCommands("p1");
  expect(useNative.getState().commandsByProject.p1).toBeUndefined();
  spy.mockRestore();
});
