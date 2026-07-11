// Regenerates crates/core/src/llm_router/model_meta_snapshot.json from
// models.dev. Run: bun scripts/models-meta/update.ts
type Meta = {
  context_window: number;
  max_output_tokens: number;
  supports_prompt_cache: boolean;
  supports_reasoning: boolean;
  display_name?: string;
  reasoning_efforts?: Array<{ value: string; label: string; description?: string }>;
  default_reasoning_effort?: string;
  cost_input: number;
  cost_output: number;
  cost_cache_read: number;
  cost_cache_write: number;
};

const api = (await (await fetch("https://models.dev/api.json")).json()) as Record<string, { models?: Record<string, any> }>;
const out: Record<string, Meta> = {};
const exactKey = (providerId: string, modelId: string) => `provider::${providerId}::model::${modelId}`;
const genericKey = (modelId: string) => `generic::${modelId}`;
for (const [providerId, provider] of Object.entries(api)) {
  for (const [id, m] of Object.entries(provider.models ?? {})) {
    const meta: Meta = {
      context_window: m.limit?.context ?? 128_000,
      max_output_tokens: m.limit?.output ?? 8_192,
      supports_prompt_cache: m.cost?.cache_read != null,
      supports_reasoning: m.reasoning === true,
      ...(typeof m.name === "string" ? { display_name: m.name } : {}),
      cost_input: m.cost?.input ?? 0,
      cost_output: m.cost?.output ?? 0,
      cost_cache_read: m.cost?.cache_read ?? 0,
      cost_cache_write: m.cost?.cache_write ?? 0,
    };
    out[exactKey(providerId, id)] = meta;
    const generic = genericKey(id);
    const prev = out[generic];
    // The same model id can appear under several providers; keep the entry
    // with the largest window (first-party listings are the most accurate).
    if (!prev || meta.context_window > prev.context_window) out[generic] = meta;
  }
}

type CodexSource = {
  models?: Array<{
    slug: string;
    display_name?: string;
    supported_reasoning_levels?: Array<{ effort: string; description?: string }>;
    default_reasoning_level?: string | null;
  }>;
};
const codex = (await Bun.file("scripts/models-meta/codex-effort-source.json").json()) as CodexSource;
for (const model of codex.models ?? []) {
  const generic = out[genericKey(model.slug)];
  const codexExactKey = exactKey("openai-oauth", model.slug);
  const existing = out[codexExactKey] ??
    generic ?? {
      context_window: 128_000,
      max_output_tokens: 8_192,
      supports_prompt_cache: false,
      supports_reasoning: true,
      cost_input: 0,
      cost_output: 0,
      cost_cache_read: 0,
      cost_cache_write: 0,
    };
  out[codexExactKey] = {
    ...existing,
    ...(model.display_name ? { display_name: model.display_name } : {}),
    supports_reasoning: true,
    reasoning_efforts: (model.supported_reasoning_levels ?? []).map((option) => ({
      value: option.effort,
      label: option.effort,
      ...(option.description ? { description: option.description } : {}),
    })),
    ...(typeof model.default_reasoning_level === "string" ? { default_reasoning_effort: model.default_reasoning_level } : {}),
  };
}
const sorted = Object.fromEntries(Object.entries(out).sort(([a], [b]) => a.localeCompare(b)));
await Bun.write("crates/core/src/llm_router/model_meta_snapshot.json", JSON.stringify(sorted, null, 1) + "\n");
console.log(`wrote ${Object.keys(sorted).length} models`);
