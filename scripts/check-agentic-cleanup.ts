import { existsSync } from "node:fs";
import { Glob } from "bun";

// Plan 6 (agentic cleanup) permanently retired the single-agent settings
// surface, the memory/Learning hub, the curator daemon, and the app
// orchestrator. This script is the permanent guard: it fails the build if any
// of that removed surface (file, RPC/command name, store accessor, or
// frontend symbol) ever comes back, while staying quiet about code that Plan
// 6 deliberately kept (per-agent OKF learning, skill-usage telemetry FIELDS,
// the `skill` view tool, `crate::learning`'s queue worker, session/message
// full-text search, etc).
const forbiddenFiles = [
  "crates/core/src/agent_settings.rs",
  "crates/core/src/orch.rs",
  "crates/core/src/api/orch_api.rs",
  "crates/core/src/harness/native/tools/app_orchestrate.rs",
  "crates/core/src/harness/native/tools/orch_block.rs",
  "crates/core/src/curator.rs",
  "crates/core/src/api/learning_api.rs",
  "crates/core/src/harness/native/tools/skill_manage.rs",
  "apps/cockpit/src-tauri/src/learning_cmd.rs",
  "apps/cockpit/src/store-agent.ts",
  "apps/cockpit/src/store-agent.test.ts",
  "apps/cockpit/src/store-orch.test.ts",
  "apps/cockpit/src/views/LearningView.tsx",
  "apps/cockpit/src/components/session/TaskStrip.tsx",
];

const scans = [
  {
    glob: "crates/core/src/**/*.{rs,sql}",
    pattern:
      /\b(get_agent_settings|set_agent_settings|read_memory|write_memory|learning_graph|curator_status|curator_rollback|orch_submit|orch_list_roots|orch_tasks|orch_cancel|orch_retry|orch_answer_block|orch_steer|app_orchestrate|orch_block|search_sessions|mod agent_settings|mod curator|skill_manage)\b/,
  },
  {
    glob: "apps/cockpit/src/**/*.{ts,tsx}",
    pattern:
      /\b(getAgentSettings|setAgentSettings|readMemory|writeMemory|learningGraph|curatorStatus|curatorRollback|orchSubmit|orchListRoots|orchTasks|orchCancel|orchRetry|orchAnswerBlock|orchSteer|searchSessions|listSkillUsage|setSkillPinned|LearningView|TaskStrip)\b/,
  },
  {
    glob: "apps/cockpit/src-tauri/src/**/*.rs",
    pattern: /\b(search_sessions|list_skill_usage|set_skill_pinned|learning_cmd)\b/,
  },
];

// Files that must legitimately keep naming a removed identifier, and are
// therefore exempt from the broad scan above (but NOT from
// `forbiddenStoreAccessors`, which always runs against store.rs directly):
//   - store.rs: migration DDL/tests name the dropped tables/columns.
//   - api/mod.rs: the dispatcher-negative test asserts these RPC method
//     names now 404, so it must keep the string literals.
//   - agentic_upgrade_compat.rs: Plan 6 Task 2's crash-order compatibility
//     proof creates a pre-migration fixture DB containing the old
//     `orch_tasks` table (and friends) specifically to prove the migration
//     drops them; it cannot do that without naming them.
const allowedHistoricalReferences = new Set([
  "crates/core/src/store.rs",
  "crates/core/src/api/mod.rs",
  "crates/core/src/agentic_upgrade_compat.rs",
]);

const failures: string[] = forbiddenFiles.filter(existsSync).map((path) => `obsolete file exists: ${path}`);

const forbiddenStoreAccessors =
  /\b(record_skill_use|record_skill_view|record_skill_patch|bump_skill_counter|set_skill_state|set_skill_pinned|mark_skill_created_by_agent|get_skill_usage|list_skill_usage|curator_last_run|insert_curator_run|finish_curator_run|list_curator_runs|create_orch_task|list_orch_tasks|get_orch_task|update_orch_task|delete_orch_task)\b/;
const storeSource = await Bun.file("crates/core/src/store.rs").text();
if (forbiddenStoreAccessors.test(storeSource)) {
  failures.push(`obsolete live Store accessor in crates/core/src/store.rs: ${forbiddenStoreAccessors.source}`);
}

for (const scan of scans) {
  for await (const path of new Glob(scan.glob).scan({ cwd: ".", onlyFiles: true })) {
    const normalized = path.replaceAll("\\", "/");
    if (allowedHistoricalReferences.has(normalized)) continue;
    const text = await Bun.file(path).text();
    if (scan.pattern.test(text)) failures.push(`obsolete symbol in ${normalized}: ${scan.pattern.source}`);
  }
}

if (failures.length > 0) {
  console.error(failures.join("\n"));
  process.exit(1);
}
console.log("agentic cleanup absence checks: PASS");
