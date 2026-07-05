import { Bot, Cloud, Cpu, Database, Globe, Key, Mail, MessageCircle, Puzzle, Search, Server, Terminal, Webhook } from "lucide-react";

// Explicit manifest-icon → lucide map (no `import *` — keeps the bundle from
// pulling in the whole icon set). Manifests are freeform strings; anything
// outside this small known set — including the canonical `icon = "github"`
// example from the plugin-sdk docs, since lucide-react dropped brand/logo
// icons — falls through to the universal `Puzzle` fallback.
//
// Shared by the sidebar's plugin menu section and the plugin detail/catalog
// screens so the two surfaces never draw a different icon for the same
// manifest `icon` string.
export const PLUGIN_ICONS: Record<string, typeof Puzzle> = {
  "message-circle": MessageCircle,
  terminal: Terminal,
  cpu: Cpu,
  globe: Globe,
  database: Database,
  search: Search,
  cloud: Cloud,
  server: Server,
  webhook: Webhook,
  key: Key,
  mail: Mail,
  bot: Bot,
};

export function pluginIcon(icon: string | null): typeof Puzzle {
  return (icon && PLUGIN_ICONS[icon]) || Puzzle;
}
