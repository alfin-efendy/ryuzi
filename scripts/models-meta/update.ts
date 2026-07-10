// Regenerates crates/core/src/llm_router/model_meta_snapshot.json from
// models.dev. Run: bun scripts/models-meta/update.ts
type Meta = {
  context_window: number;
  max_output_tokens: number;
  supports_prompt_cache: boolean;
  supports_reasoning: boolean;
};

const api = (await (await fetch("https://models.dev/api.json")).json()) as Record<
  string,
  { models?: Record<string, any> }
>;
const out: Record<string, Meta> = {};
for (const provider of Object.values(api)) {
  for (const [id, m] of Object.entries(provider.models ?? {})) {
    const meta: Meta = {
      context_window: m.limit?.context ?? 128_000,
      max_output_tokens: m.limit?.output ?? 8_192,
      supports_prompt_cache: m.cost?.cache_read != null,
      supports_reasoning: m.reasoning === true,
    };
    const prev = out[id];
    // The same model id can appear under several providers; keep the entry
    // with the largest window (first-party listings are the most accurate).
    if (!prev || meta.context_window > prev.context_window) out[id] = meta;
  }
}
const sorted = Object.fromEntries(Object.entries(out).sort(([a], [b]) => a.localeCompare(b)));
await Bun.write(
  "crates/core/src/llm_router/model_meta_snapshot.json",
  JSON.stringify(sorted, null, 1) + "\n",
);
console.log(`wrote ${Object.keys(sorted).length} models`);
