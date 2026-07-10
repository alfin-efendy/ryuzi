# Cost & context accounting

Per-model USD rates live on `ModelMeta` (`crates/core/src/llm_router/model_meta.rs`):
`cost_input`, `cost_output`, `cost_cache_read`, `cost_cache_write` — USD per
1M tokens, sourced from models.dev `cost.*` and bundled in
`model_meta_snapshot.json` (regenerate with `bun scripts/models-meta/update.ts`).
`ModelMeta::cost_usd` prices one request's four disjoint token buckets
(Anthropic reports non-cached input, cache-read, and cache-creation
separately, so each is priced at its own rate).

The native `ContextManager` observes all four billed buckets per response.
Cost accumulates in exactly one place — right after a fresh
`cm.commit_response()` in `harness/native/runner.rs` — into a per-model token
tally stored in the `session_context` JSON payload under `"models"`. The tally
holds TOKENS only; dollars are computed from current rates at emit time, so a
price-table correction retroactively fixes history. Display re-emits (context
overflow, pre-turn seed, manual `/compact`) and resume go through a
display-only path that refreshes the context snapshot without re-accumulating
(their `cm.last_*` still hold the previous response), so no response is ever
counted twice. Sub-agent/ephemeral loops (`emit=false`) accumulate nothing.
`CoreEvent::SessionCost { session_pk, total_usd, models }` is emitted alongside
`ContextUsage`; the tally survives resume.

Cockpit renders a `ContextRing` donut (fills with % context used,
calm→amber→red) in the composer; clicking it opens `SessionCostPanel` with the
context detail (active / usable / full window, cache reads) and the per-model
dollar/token breakdown. Cost that is zero or absent renders `—`; sub-cent
totals render `<$0.01`. Sessions that predate this feature have no `"models"`
tally and show `—` until their next turn.

Not covered: spend caps/budgets; the 14-day provider usage chart
(`usage_daily`), which remains input/output-only and per-connection.
