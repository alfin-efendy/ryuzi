import { beforeEach, expect, spyOn, test } from "bun:test";
import { useStore } from "./store";
import { commands } from "./bindings";
import type { CoreEvent } from "./bindings";
import { LOCAL_RUNNER, sessKey } from "@/lib/session-key";

const kHome = sessKey(LOCAL_RUNNER, "home-1");
const kS1 = sessKey(LOCAL_RUNNER, "s1");

beforeEach(() => {
  useStore.setState({ orchTasks: {}, focusedSession: null, loaded: {}, transcripts: {}, lastSeq: {} });
});

test("orchTaskChanged upserts a chip status without dropping siblings", () => {
  useStore
    .getState()
    .applyCoreEvent({ kind: "orchTaskChanged", task_id: "ot-1", root_id: "ot-root", status: "running" } as CoreEvent, LOCAL_RUNNER);
  useStore
    .getState()
    .applyCoreEvent({ kind: "orchTaskChanged", task_id: "ot-2", root_id: "ot-root", status: "todo" } as CoreEvent, LOCAL_RUNNER);
  useStore
    .getState()
    .applyCoreEvent({ kind: "orchTaskChanged", task_id: "ot-1", root_id: "ot-root", status: "done" } as CoreEvent, LOCAL_RUNNER);

  const tasks = useStore.getState().orchTasks["ot-root"];
  expect(tasks.find((t) => t.id === "ot-1")?.status).toBe("done");
  expect(tasks.find((t) => t.id === "ot-2")?.status).toBe("todo");
});

test("a root reports its own status under its own id (root_id: null)", () => {
  useStore
    .getState()
    .applyCoreEvent({ kind: "orchTaskChanged", task_id: "ot-root", root_id: null, status: "decomposing" } as CoreEvent, LOCAL_RUNNER);

  const tasks = useStore.getState().orchTasks["ot-root"];
  expect(tasks).toHaveLength(1);
  expect(tasks[0]).toMatchObject({ id: "ot-root", rootId: null, status: "decomposing" });
});

test("terminal/blocked status for a child refetches the focused session's transcript", () => {
  useStore.setState({ focusedSession: { runnerId: LOCAL_RUNNER, pk: "home-1" }, loaded: { [kHome]: true } });
  const listMessages = spyOn(commands, "listMessages").mockResolvedValue({ status: "ok", data: [] });

  useStore
    .getState()
    .applyCoreEvent({ kind: "orchTaskChanged", task_id: "ot-1", root_id: "ot-root", status: "done" } as CoreEvent, LOCAL_RUNNER);

  expect(listMessages).toHaveBeenCalledWith(LOCAL_RUNNER, "home-1");
  listMessages.mockRestore();
});

test("a running/todo status does not trigger a refetch", () => {
  useStore.setState({ focusedSession: { runnerId: LOCAL_RUNNER, pk: "home-1" }, loaded: { [kHome]: true } });
  const listMessages = spyOn(commands, "listMessages").mockResolvedValue({ status: "ok", data: [] });

  useStore
    .getState()
    .applyCoreEvent({ kind: "orchTaskChanged", task_id: "ot-1", root_id: "ot-root", status: "running" } as CoreEvent, LOCAL_RUNNER);

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
  useStore.setState({ loaded: { [kS1]: true }, transcripts: { [kS1]: [] }, lastSeq: { [kS1]: 0 } });

  // hydrateTranscript no-ops once a session is already loaded...
  const noopFetcher = async () => {
    throw new Error("hydrateTranscript should not fetch when already loaded");
  };
  await useStore.getState().hydrateTranscript(LOCAL_RUNNER, "s1", noopFetcher as never);

  // ...but refetchTranscript forces the fetch through regardless.
  let called = false;
  await useStore.getState().refetchTranscript(LOCAL_RUNNER, "s1", async () => {
    called = true;
    return [];
  });
  expect(called).toBe(true);
});

test("answering a block calls orchAnswerBlock with the task id", async () => {
  const spy = spyOn(commands, "orchAnswerBlock").mockResolvedValue({ status: "ok", data: true });
  await useStore.getState().orchAnswerBlock("ot-7", "use 8080");
  expect(spy).toHaveBeenCalledWith("ot-7", "use 8080");
  spy.mockRestore();
});

test("startOrchestration submits when both a project and a focused home session are present", async () => {
  useStore.setState({ selectedProjectId: "p1", focusedSession: { runnerId: LOCAL_RUNNER, pk: "home-1" } });
  const submit = spyOn(commands, "orchSubmit").mockResolvedValue({ status: "ok", data: "root-1" });
  await expect(useStore.getState().startOrchestration("fix the bug")).resolves.toBe(true);
  expect(submit).toHaveBeenCalledWith("p1", "fix the bug", true, "home-1");
  submit.mockRestore();
});

test("startOrchestration is a no-op without both an attached project and a focused home session", async () => {
  const submit = spyOn(commands, "orchSubmit");

  useStore.setState({ selectedProjectId: null, focusedSession: { runnerId: LOCAL_RUNNER, pk: "home-1" } });
  await expect(useStore.getState().startOrchestration("fix the bug")).resolves.toBe(false);

  useStore.setState({ selectedProjectId: "p1", focusedSession: null });
  await expect(useStore.getState().startOrchestration("fix the bug")).resolves.toBe(false);

  expect(submit).not.toHaveBeenCalled();
  submit.mockRestore();
});

test("send steers a live orchestration and does not also send a normal chat turn", async () => {
  useStore.setState({ sessions: [] });
  const steer = spyOn(commands, "orchSteer").mockResolvedValue({ status: "ok", data: "noted" });
  const cont = spyOn(commands, "continueSession").mockResolvedValue({ status: "ok", data: null });
  const steerSession = spyOn(commands, "steerSession").mockResolvedValue({ status: "ok", data: true });

  await expect(
    useStore.getState().send(LOCAL_RUNNER, "home-1", { text: "cancel the build task", context: null, attachments: [], git: null }),
  ).resolves.toBe(true);
  expect(steer).toHaveBeenCalledWith("home-1", "cancel the build task");
  expect(cont).not.toHaveBeenCalled();
  expect(steerSession).not.toHaveBeenCalled();

  steer.mockRestore();
  cont.mockRestore();
  steerSession.mockRestore();
});

test("send falls through to the normal path when orchSteer reports no live orchestration", async () => {
  useStore.setState({ sessions: [] });
  const steer = spyOn(commands, "orchSteer").mockResolvedValue({ status: "ok", data: "noOrchestration" });
  const cont = spyOn(commands, "continueSession").mockResolvedValue({ status: "ok", data: null });
  const listProjects = spyOn(commands, "listProjects").mockResolvedValue({ status: "ok", data: [] });
  const listSessions = spyOn(commands, "listSessions").mockResolvedValue({ status: "ok", data: [] });
  const listGateways = spyOn(commands, "listGateways").mockResolvedValue({ status: "ok", data: [] });

  await expect(useStore.getState().send(LOCAL_RUNNER, "s1", { text: "hi", context: null, attachments: [], git: null })).resolves.toBe(true);
  expect(steer).toHaveBeenCalledWith("s1", "hi");
  expect(cont).toHaveBeenCalledWith(LOCAL_RUNNER, "s1", { text: "hi", context: null, attachments: [], git: null });

  steer.mockRestore();
  cont.mockRestore();
  listProjects.mockRestore();
  listSessions.mockRestore();
  listGateways.mockRestore();
});

test("send falls through to the normal path when orchSteer itself errors", async () => {
  useStore.setState({ sessions: [] });
  const steer = spyOn(commands, "orchSteer").mockResolvedValue({ status: "error", error: { message: "unreachable" } });
  const cont = spyOn(commands, "continueSession").mockResolvedValue({ status: "ok", data: null });
  const listProjects = spyOn(commands, "listProjects").mockResolvedValue({ status: "ok", data: [] });
  const listSessions = spyOn(commands, "listSessions").mockResolvedValue({ status: "ok", data: [] });
  const listGateways = spyOn(commands, "listGateways").mockResolvedValue({ status: "ok", data: [] });

  await expect(useStore.getState().send(LOCAL_RUNNER, "s1", { text: "hi", context: null, attachments: [], git: null })).resolves.toBe(true);
  expect(cont).toHaveBeenCalledWith(LOCAL_RUNNER, "s1", { text: "hi", context: null, attachments: [], git: null });

  steer.mockRestore();
  cont.mockRestore();
  listProjects.mockRestore();
  listSessions.mockRestore();
  listGateways.mockRestore();
});
