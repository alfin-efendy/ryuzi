import type { SettingsStore } from "./store";
import type { ConfigField, ProviderCatalog } from "../providers/types";
import { GLOBAL_FIELDS } from "./schema";

export function csv(s: string | undefined): string[] {
  return (s ?? "")
    .split(",")
    .map((x) => x.trim())
    .filter(Boolean);
}

export function requiredMissingFields(settings: SettingsStore, cat: ProviderCatalog): ConfigField[] {
  const out: ConfigField[] = [];
  for (const f of GLOBAL_FIELDS) if (f.required && settings.get(f.key) === undefined) out.push(f);
  for (const id of csv(settings.get("enabled_gateways")))
    for (const f of cat.gateway(id)?.fields ?? []) if (f.required && settings.get(f.key) === undefined) out.push(f);
  for (const id of csv(settings.get("enabled_runtimes")))
    for (const f of cat.runtime(id)?.fields ?? []) if (f.required && settings.get(f.key) === undefined) out.push(f);
  return out;
}

export function missingRequiredSettings(settings: SettingsStore, cat: ProviderCatalog): string[] {
  return requiredMissingFields(settings, cat).map((f) => f.key);
}

export function isConfigured(settings: SettingsStore, cat: ProviderCatalog): boolean {
  return (
    csv(settings.get("enabled_gateways")).length > 0 &&
    csv(settings.get("enabled_runtimes")).length > 0 &&
    missingRequiredSettings(settings, cat).length === 0
  );
}
