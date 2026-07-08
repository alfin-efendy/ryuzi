import type { CatalogEntry } from "@/bindings";

/**
 * True when the provider signs in via a device-code flow: Kiro (category
 * "device") or an RFC 8628 device-grant provider (category "oauth" +
 * usesDeviceGrant, e.g. qwen / github-copilot). Redirect-loopback OAuth
 * providers (anthropic-oauth / openai-oauth) return false.
 */
export function usesDeviceSignin(entry: CatalogEntry): boolean {
  return entry.category === "device" || entry.usesDeviceGrant;
}
