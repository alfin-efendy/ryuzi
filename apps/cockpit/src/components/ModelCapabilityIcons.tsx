import { AudioLines, Eye, FileText, Image as ImageIcon } from "lucide-react";

export type ModelCapability = "vision" | "pdf" | "audio" | "image";

export function modelCapabilities(model: string): ModelCapability[] {
  const m = model.toLowerCase();
  const vision = includesAny(m, [
    "gpt-4o",
    "gpt-4.1",
    "gpt-5",
    "o3",
    "o4",
    "o5",
    "claude",
    "gemini",
    "vision",
    "pixtral",
    "llava",
    "qwen-vl",
    "qwen2-vl",
    "qwen3-vl",
    "grok-4",
  ]);
  const audio = includesAny(m, ["audio", "realtime", "transcribe", "transcription", "whisper", "tts", "gpt-4o"]);
  const image = includesAny(m, ["gpt-image", "dall-e", "imagen", "image-generation"]) || (m.includes("image") && !m.includes("image_url"));
  const caps: ModelCapability[] = [];
  if (vision) caps.push("vision");
  if (vision || includesAny(m, ["pdf", "document"])) caps.push("pdf");
  if (audio) caps.push("audio");
  if (image) caps.push("image");
  return caps;
}

function includesAny(value: string, needles: string[]): boolean {
  return needles.some((needle) => value.includes(needle));
}

const META: Record<ModelCapability, { label: string; icon: typeof Eye }> = {
  vision: { label: "Vision", icon: Eye },
  pdf: { label: "PDF", icon: FileText },
  audio: { label: "Audio", icon: AudioLines },
  image: { label: "Image generation", icon: ImageIcon },
};

export function ModelCapabilityIcons({ model, compact = false }: { model: string; compact?: boolean }) {
  const caps = modelCapabilities(model);
  if (caps.length === 0) return null;
  return (
    <span className="inline-flex shrink-0 items-center gap-1">
      {caps.map((cap) => {
        const Icon = META[cap].icon;
        return (
          <span
            key={cap}
            title={META[cap].label}
            aria-label={META[cap].label}
            className="inline-flex size-5 items-center justify-center rounded-md border border-border bg-background text-muted-foreground"
          >
            <Icon aria-hidden size={compact ? 10 : 11} strokeWidth={2} className={compact ? "size-2.5" : "size-3"} />
          </span>
        );
      })}
    </span>
  );
}
