# Native Skill Paths, Task Schema, and Startup Transcript Order

**Date:** 2026-07-11
**Status:** Approved design; awaiting written-spec review

## Goal

Fix three connected native-runtime/Cockpit defects:

1. A skill body that refers to an adjacent companion file (for example,
   `skills/brainstorming/visual-companion.md`) cannot currently be read because
   `read` only resolves paths relative to the worktree or an attachment
   directory.
2. Models can emit both the single-task `prompt` form and the batch `tasks`
   form when calling the `task` tool, which is correctly rejected at runtime
   but is not prevented by its tool schema.
3. During a newly created project session, startup status rows (for example,
   `Creating worktree…`) can arrive before the durable user-message row and
   therefore render above the user's bubble.

## Scope

The change is limited to `ryuzi-core` native skill/tool behavior and Cockpit
transcript shaping. It does not change worktree startup timing, event ordering,
SQLite message ordering, or the external session lifecycle API.

## Design

### 1. Read companion files from an installed skill

Extend the native `Skill` metadata held by `SkillRegistry` with the directory
that contains its `SKILL.md`. Every registry discovery path—worktree skills,
global skills, and plugin-contributed skill directories—sets this directory
when constructing a skill.

The native `read` tool recognizes a virtual skill path in this exact form:

```text
skills/<skill-name>/<relative-path>
```

For such a path, it:

1. resolves `<skill-name>` through a freshly loaded `SkillRegistry`, using the
   same worktree and plugin skill directories as the `skill` tool;
2. resolves `<relative-path>` below the discovered skill directory; and
3. applies the existing sandbox containment guarantees so absolute paths,
   parent traversal, and symlinks escaping the skill directory are rejected.

After resolution, the existing text/image read path, limits, MIME validation,
and output truncation are reused unchanged. Worktree-relative and
attachment-relative file reads preserve their current behavior and priority.

If the skill does not exist, the virtual path is malformed, or the companion
file is missing, `read` returns an ordinary tool error rather than falling back
to an unrelated worktree path.

Tests cover reading an adjacent file from a leaf skill directory, containment
rejection for traversal/escape paths, and preservation of normal worktree
reads.

### 2. Make `task` forms mutually exclusive in its schema

Keep the runtime validation strict: a payload containing both top-level
`prompt` and `tasks` remains an error and executes neither form.

Express the same rule in the native tool JSON schema using two exclusive
alternatives:

- **Single form:** requires top-level `prompt`; permits optional
  `subagent_type` and `description`; forbids `tasks`.
- **Batch form:** requires non-empty `tasks`; each element requires `prompt`
  and may include `subagent_type`; forbids top-level `prompt` and
  `subagent_type`.

Update the tool description and built-in orchestrator prompt to say that one
call must choose exactly one form. This reduces invalid provider calls while
the existing runtime branch remains the authoritative defense when a provider
ignores schema constraints.

Tests inspect the generated schema's exclusive alternatives and retain the
existing strict runtime test for mixed input. Tests also confirm valid single
and batch forms continue to dispatch normally.

### 3. Render startup progress after the initial user bubble

Keep status rows persisted and emitted in their real lifecycle order: startup
may still produce `Creating worktree…`, branch, and connection statuses before
the native runner persists its user row.

In Cockpit's pure `buildTranscript` presentation layer, when the live final
turn begins with activity/status rows and then first reaches a user row,
reorder only the visual groups to render:

```text
user bubble → startup activity/status → remaining live assistant output
```

The startup activity group retains its internal order. Activity already after a
user bubble is unchanged. Completed turns retain the existing summary behavior;
this reordering only addresses the live startup turn, where the timing gap is
visible.

A focused transcript unit test starts with a `Creating worktree…` status row
followed by a user row and asserts the live visual block order is `user`, then
`activity`. Existing live and completed-turn grouping tests protect ordinary
activity ordering and summary rendering.

## Error Handling and Compatibility

- Skill virtual paths are only an additional read root; they cannot expand
  write, edit, shell, or attachment permissions.
- Missing/invalid skill paths return explicit read errors and do not silently
  resolve elsewhere.
- Mixed `task` payloads remain strict errors. Schema constraints are advisory
  to providers; runtime validation remains required.
- Startup lifecycle concurrency and durable event/data sequence do not change;
  only Cockpit rendering order changes for the defined live-startup shape.

## Verification

Run targeted tests for:

- `crates/core/src/harness/native/skills.rs` and
  `crates/core/src/harness/native/tools/read.rs`;
- `crates/core/src/harness/native/tools/task.rs`;
- `apps/cockpit/src/lib/transcript.test.ts`.

Then run `cargo fmt`, `cargo test -p ryuzi-core`, and the targeted Cockpit Bun
test. Run `bun run --cwd apps/cockpit build` if the frontend change is broad
beyond the pure transcript test; otherwise report why the targeted test is the
chosen verification.
