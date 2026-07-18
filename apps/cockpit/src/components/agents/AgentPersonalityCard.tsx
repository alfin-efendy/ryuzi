import { useEffect, useState } from "react";
import { Button, Combobox, type ComboboxGroup, type ComboboxOption, SettingsCard, SettingsCardRow, SettingsCardTitle, Textarea } from "@ryuzi/ui";
import type { AgentDetailInfo, AgentPersonalityInfo } from "@/bindings";
import { useAgents } from "@/store-agents";
import { mutationFromDetail } from "./agentMutation";

// Mirrors crates/core/src/agents/personality.rs's `PersonalityPreset` catalog
// and baked-in prompt text: a closed set of professional + expressive
// presets, plus `custom` for free-form user text.
const PROFESSIONAL_PRESETS: ComboboxOption[] = [
  { value: "helpful", label: "Helpful", description: "You are a helpful, direct assistant. Prioritize clarity, accuracy, and being genuinely useful." },
  { value: "concise", label: "Concise", description: "You are concise. Prefer short, information-dense answers over padding or repetition." },
  {
    value: "technical",
    label: "Technical",
    description: "You are a technical expert. Use precise terminology, cite specifics, and favor rigor over hand-waving.",
  },
  { value: "creative", label: "Creative", description: "You are imaginative and expressive. Bring fresh framing, vivid language, and original ideas." },
  { value: "teacher", label: "Teacher", description: "You are a patient teacher. Explain reasoning step by step and check for understanding." },
  {
    value: "philosopher",
    label: "Philosopher",
    description: "You are a thoughtful philosopher. Explore questions from multiple angles and examine assumptions.",
  },
];

const EXPRESSIVE_PRESETS: ComboboxOption[] = [
  {
    value: "kawaii",
    label: "Kawaii",
    description: "You are cheerful and cute (kawaii) in tone, using warm and playful language while staying helpful.",
  },
  {
    value: "catgirl",
    label: "Catgirl",
    description: "You are a playful catgirl persona: energetic, affectionate, and sprinkled with cat-like flourishes, while staying helpful.",
  },
  { value: "pirate", label: "Pirate", description: "You are a swashbuckling pirate. Speak with pirate slang and flair while staying helpful and accurate." },
  {
    value: "shakespeare",
    label: "Shakespeare",
    description: "You speak in the style of Shakespearean English: archaic diction and dramatic flourish, while staying helpful and accurate.",
  },
  { value: "surfer", label: "Surfer", description: "You are a laid-back surfer. Speak casually and chill, while staying helpful and accurate." },
  {
    value: "noir",
    label: "Noir",
    description: "You speak like a hardboiled noir detective: terse, atmospheric, and wry, while staying helpful and accurate.",
  },
  { value: "uwu", label: "Uwu", description: "You speak in an uwu/owo internet-cute style, while staying helpful and accurate." },
  {
    value: "hype",
    label: "Hype",
    description: "You are hype and enthusiastic, bringing high energy and encouragement, while staying helpful and accurate.",
  },
];

const CUSTOM_PRESET: ComboboxOption = { value: "custom", label: "Custom", description: "Write your own personality instructions." };

// Empty group label renders no header (see Combobox's headingless-group
// convention) — Custom sits ungrouped below the two labeled sections.
const PRESET_GROUPS: ComboboxGroup[] = [
  { label: "Professional", options: PROFESSIONAL_PRESETS },
  { label: "Expressive", options: EXPRESSIVE_PRESETS },
  { label: "", options: [CUSTOM_PRESET] },
];

const ALL_PRESETS: ComboboxOption[] = [...PROFESSIONAL_PRESETS, ...EXPRESSIVE_PRESETS, CUSTOM_PRESET];

export function AgentPersonalityCard({ detail }: { detail: AgentDetailInfo }) {
  const saving = useAgents((state) => state.saving);
  const [personality, setPersonality] = useState<AgentPersonalityInfo>(detail.personality);

  useEffect(() => setPersonality(detail.personality), [detail]);

  const isCustom = personality.preset === "custom";
  const description = ALL_PRESETS.find((preset) => preset.value === personality.preset)?.description ?? null;
  const customText = personality.custom ?? "";
  const customBlank = isCustom && customText.trim().length === 0;

  const selectPreset = (preset: string) => {
    setPersonality({ preset, custom: preset === "custom" ? customText : null });
  };

  const save = () => {
    if (saving || customBlank) return;
    void useAgents.getState().update(detail.summary.id, {
      ...mutationFromDetail(detail),
      personality: isCustom ? { preset: "custom", custom: customText.trim() } : { preset: personality.preset, custom: null },
    });
  };

  return (
    <SettingsCard>
      <div className="border-b border-border px-[18px] py-3.5">
        <SettingsCardTitle>Personality</SettingsCardTitle>
      </div>
      <SettingsCardRow className="gap-4">
        <span className="w-40 shrink-0 text-[13px] font-medium">Preset</span>
        <Combobox
          aria-label="Personality preset"
          className="min-w-0 flex-1"
          options={PRESET_GROUPS}
          value={personality.preset}
          onValueChange={selectPreset}
          disabled={saving}
        />
      </SettingsCardRow>
      {isCustom ? (
        <SettingsCardRow className="items-start gap-4">
          <span className="w-40 shrink-0 pt-2 text-[13px] font-medium">Custom instructions</span>
          <Textarea
            aria-label="Custom personality"
            className="min-w-0 flex-1"
            rows={4}
            value={customText}
            disabled={saving}
            onChange={(event) => setPersonality((current) => ({ ...current, custom: event.target.value }))}
          />
        </SettingsCardRow>
      ) : description !== null ? (
        <SettingsCardRow>
          <span className="min-w-0 flex-1 text-xs text-muted-foreground">{description}</span>
        </SettingsCardRow>
      ) : null}
      <div className="flex justify-end border-t border-border px-[18px] py-3">
        <Button disabled={saving || customBlank} onClick={save}>
          Save personality
        </Button>
      </div>
    </SettingsCard>
  );
}
