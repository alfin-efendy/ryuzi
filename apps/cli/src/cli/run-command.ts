// apps/router/src/cli/run-command.ts
import { parseArgs } from "node:util";
import { dirname, resolve } from "node:path";
import type { CoreEvent } from "@harness/protocol";
import { PERM_MODES, type PermMode } from "@harness/protocol";
import type { CliDeps } from "./run";
import { openDb, ProjectsStore, SessionsStore, SettingsStore, ControlPlane, expandHome, catalog, csv } from "@harness/core";

export async function cmdRun(args: string[], deps: CliDeps): Promise<number> {
  let dir: string | undefined, prompt: string | undefined, model: string | undefined, effort: string | undefined, mode: string | undefined;
  try {
    const parsed = parseArgs({
      args,
      allowPositionals: false,
      options: {
        dir: { type: "string" },
        prompt: { type: "string" },
        model: { type: "string" },
        effort: { type: "string" },
        mode: { type: "string" },
      },
    });
    ({ dir, prompt, model, effort, mode } = parsed.values);
  } catch (e) {
    deps.io.err((e as Error).message);
    return 1;
  }
  if (!dir || !prompt) {
    deps.io.err("usage: hr run --dir <git-repo> --prompt <text> [--model x] [--effort y] [--mode m]");
    return 1;
  }
  if (mode && !(PERM_MODES as readonly string[]).includes(mode)) {
    deps.io.err(`--mode must be one of: ${PERM_MODES.join(", ")}`);
    return 1;
  }

  const workdir = resolve(expandHome(dir));
  const db = openDb(deps.dbPath);
  const projects = new ProjectsStore(db);
  const settings = new SettingsStore(db);
  const cp = new ControlPlane({
    projects,
    sessions: new SessionsStore(db),
    settings,
    // one-shot CLI path never passes attachments, so the attachments-root divergence from settings.workdir_root is inert
    workdirRoot: dirname(workdir),
  });
  const defaultRuntime = settings.get("default_runtime") || "claude-code";
  if (deps.harnessFactory) {
    cp.harnesses.register(defaultRuntime, deps.harnessFactory);
  } else {
    for (const id of csv(settings.get("enabled_runtimes"))) {
      const d = catalog.runtime(id);
      if (d) cp.harnesses.register(id, () => d.build());
    }
  }

  if (!projects.get(workdir)) {
    projects.insert({
      projectId: workdir,
      name: workdir.split("/").pop() ?? workdir,
      workdir,
      harness: defaultRuntime,
      model,
      effort,
      permMode: (mode as PermMode | undefined) ?? "default",
    });
  }

  let failed = false;
  cp.subscribe((e: CoreEvent) => {
    if (e.kind === "status") deps.io.out(`· ${e.text}`);
    else if (e.kind === "text") deps.io.out(e.text);
    else if (e.kind === "result") deps.io.out(`✓ done`);
    else if (e.kind === "error") {
      failed = true;
      deps.io.err(`✗ ${e.message}`);
    }
  });

  await cp.startSession({ projectId: workdir, prompt, actor: "cli" });
  return failed ? 1 : 0;
}
