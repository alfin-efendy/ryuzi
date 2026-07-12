# Plan 2 Task 8 Implementer Report

## Scope

Finalized the inherited Task 8 work without adding RPC/UI/session ownership or delegation surfaces.

## Inherited RED

The inherited worktree contained the Task 8 RED coverage described by the brief:

- `agents::registry::tests::delete_blocks_queue_discards_rows_and_rolls_back_block_on_file_failure`
- `agents::learning_queue::tests::worker_retries_failure_then_delivers_in_agent_sequence`

These originally failed before registry deletion was connected to queue block/unblock/discard and before the queue worker existed. The current session inherited that RED milestone and completed/finalized GREEN.

## Implementation

- Composed one `AgentPersistence` graph and attached its exact registry, knowledge store, and learning queue to `ControlPlane` and every `ApiState` constructor/test fixture.
- Wired runner and Cockpit daemon/API callers to reuse daemon persistence handles.
- Coordinated deletion with queue blocking, rollback unblock, and post-commit discard while preserving filesystem transaction ordering.
- Replaced the native nudge producer's legacy `background_events(kind='learning')` assertions/path with durable per-agent queue rows.
- Kept one daemon-hosted learning worker, tracked and aborted through normal start-failure and stop lifecycle paths.
- Audited worker spawning: the sole product spawn is in `build_daemon`; tests use tracked inert handles and stop/rollback coverage confirms no task leaks.

## Final GREEN

Passed:

- `cargo check -p ryuzi-core --all-targets`
- `cargo check -p ryuzi-runner`
- `cargo check -p ryuzi-cockpit`
- `cargo test -p ryuzi-core agents::registry:: -- --nocapture` (17 passed)
- `cargo test -p ryuzi-core agents::learning_queue:: -- --nocapture` (9 passed)
- `cargo test -p ryuzi-core harness::native::runner::tests::finalizer_enqueues_a_learning_row_every_nudge_interval -- --nocapture` (1 passed)
- `cargo test -p ryuzi-core api:: --quiet` (passed)
- Focused daemon lifecycle tests:
  - `daemon_hosts_and_stop_aborts_scheduler_orch_rail_learning_and_curator_loops`
  - `daemon_stop_is_idempotent_and_stops_each_gateway_once`
  - `daemon_start_rolls_back_started_gateways_and_aborts_handles_on_later_failure`
- `cargo fmt --all -- --check`
- `cargo clippy -p ryuzi-core --lib -- -D warnings`

The broad parallel `cargo test -p ryuzi-core daemon:: -- --nocapture` run had four unrelated timing/environment failures; rerunning the filesystem failure individually passed, while three pre-existing approval/reconcile timing tests remained outside Task 8. Task 8's focused daemon worker lifecycle tests pass.

`cargo check --all-targets` emits inherited test-only extension warnings; the requested production-lib clippy gate is clean.
