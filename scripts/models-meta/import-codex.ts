// Prunes Codex's models-manager catalog to provider-owned effort metadata.
// Run: bun scripts/models-meta/import-codex.ts <models.json> <output.json>
type SourceModel = {
  slug: string;
  display_name?: string;
  supported_reasoning_levels?: Array<{ effort: string; description?: string }>;
  default_reasoning_level?: string | null;
};

const [, , input, output] = Bun.argv;
if (!input || !output) {
  throw new Error("usage: import-codex.ts <input> <output>");
}

const source = (await Bun.file(input).json()) as { models?: SourceModel[] };
const models = (source.models ?? [])
  .filter((model) => typeof model.slug === "string" && model.slug.length > 0)
  .map((model) => ({
    slug: model.slug,
    display_name: typeof model.display_name === "string" ? model.display_name : undefined,
    supported_reasoning_levels: (model.supported_reasoning_levels ?? [])
      .filter((option) => typeof option.effort === "string" && option.effort.length > 0)
      .map((option) => ({
        effort: option.effort,
        description: typeof option.description === "string" ? option.description : undefined,
      })),
    default_reasoning_level:
      typeof model.default_reasoning_level === "string" ? model.default_reasoning_level : null,
  }));

await Bun.write(output, `${JSON.stringify({ models }, null, 2)}\n`);
console.log(`wrote ${models.length} Codex model effort records`);
