import { beforeEach, expect, spyOn, test } from "bun:test";
import { useStore } from "./store";
import { commands } from "./bindings";
import type { CoreEvent } from "./bindings";

beforeEach(() => {
  useStore.setState({ orchTasks: {}, focusedSessionPk: null, loaded: {}, transcripts: {}, lastSeq: {} });
});

test("orchTaskChanged upserts a chip status without dropping siblings", () => {
  useStore.getState().applyCoreEvent({ kind: "orchTaskChanged", task_id: "ot-1", root_id: "ot-root", status: "running" } as CoreEvent);
  useStore.getState().applyCoreEvent({ kind: "orchTaskChanged", task_id: "ot-2", root_id: "ot-root", status: "todo" } as CoreEvent);
  useStore.getState().applyCoreEvent({ kind: "orchTaskChanged", task_id: "ot-1", root_id: "ot-root", status: "done" } as CoreEvent);

  const tasks = useStore.getState().orchTasks["ot-root"];
  expect(tasks.find((t) => t.id === "ot-1")?.status).toBe("done");
  expect(tasks.find((t) => t.id === "ot-2")?.status).toBe("todo");
});

test("a root reports its own status under its own id (root_id: null)", () => {
  useStore.getState().applyCoreEvent({ kind: "orchTaskChanged", task_id: "ot-root", root_id: null, status: "decomposing" } as CoreEvent);

  const tasks = useStore.getState().orchTasks["ot-root"];
  expect(tasks).toHaveLength(1);
  expect(tasks[0]).toMatchObject({ id: "ot-root", rootId: null, status: "decomposing" });
});

test("terminal/blocked status for a child refetches the focused session's transcript", () => {
  useStore.setState({ focusedSessionPk: "home-1", loaded: { "home-1": true } });
  const listMessages = spyOn(commands, "listMessages").mockResolvedValue({ status: "ok", data: [] });

  useStore.getState().applyCoreEvent({ kind: "orchTaskChanged", task_id: "ot-1", root_id: "ot-root", status: "done" } as CoreEvent);

  expect(listMessages).toHaveBeenCalledWith("home-1");
  listMessages.mockRestore();
});

test("a running/todo status does not trigger a refetch", () => {
  useStore.setState({ focusedSessionPk: "home-1", loaded: { "home-1": true } });
  const listMessages = spyOn(commands, "listMessages").mockResolvedValue({ status: "ok", data: [] });

  useStore.getState().applyCoreEvent({ kind: "orchTaskChanged", task_id: "ot-1", root_id: "ot-root", status: "running" } as CoreEvent);

  expect(listMessages).not.toHaveBeenCalled();
  listMessages.mockRestore();
});

test("loadOrchTasks stores the full rows returned by commands.orchTasks", async () => {
  const orchTasksSpy = spyOn(commands, "orchTasks").mockResolvedValue({
    status: "ok",
    data: [
      {
        id: "ot-1",
        rootId: "ot-root",
        projectId: "p1",
        title: "Write tests",
        body: "",
        agent: "build",
        status: "running",
        sessionPk: "worker-1",
        result: null,
        error: null,
        createdAt: 1,
        finishedAt: null,
        homeSessionPk: "home-1",
        consecutiveFailures: 0,
        gaveUp: false,
        steerNote: null,
      },
    ],
  } as never);

  await useStore.getState().loadOrchTasks("ot-root");

  expect(orchTasksSpy).toHaveBeenCalledWith("ot-root");
  expect(useStore.getState().orchTasks["ot-root"]).toHaveLength(1);
  expect(useStore.getState().orchTasks["ot-root"][0].title).toBe("Write tests");
  orchTasksSpy.mockRestore();
});

test("refetchTranscript bypasses the loaded short-circuit that hydrateTranscript honors", async () => {
  useStore.setState({ loaded: { s1: true }, transcripts: { s1: [] }, lastSeq: { s1: 0 } });

  // hydrateTranscript no-ops once a session is already loaded...
  const noopFetcher = async () => {
    throw new Error("hydrateTranscript should not fetch when already loaded");
  };
  await useStore.getState().hydrateTranscript("s1", noopFetcher as never);

  // ...but refetchTranscript forces the fetch through regardless.
  let called = false;
  await useStore.getState().refetchTranscript("s1", async () => {
    called = true;
    return [];
  });
  expect(called).toBe(true);
});
