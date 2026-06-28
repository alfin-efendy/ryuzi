import type { ConfigField, ProviderCatalog } from "../providers/types";
import { catalog } from "../providers/catalog";

export const GLOBAL_FIELDS: ConfigField[] = [
  {
    key: "workdir_root",
    label: "Workdir root",
    required: true,
    help: "Parent directory where project repos live",
    example: "/home/you/repos",
  },
  { key: "default_model", label: "Default model", help: "Default model for new projects (blank = harness default)" },
  {
    key: "default_effort",
    label: "Default effort",
    default: "medium",
    help: "Default reasoning effort for new projects",
    example: "medium",
  },
  {
    key: "default_perm_mode",
    label: "Default permission mode",
    type: "enum",
    oneOf: ["default", "acceptEdits", "bypassPermissions"],
    default: "default",
    help: "Default approval mode for new projects",
  },
  { key: "admin_role_ids", label: "Admin role IDs", help: "Comma-separated role IDs allowed to administer (gateway-specific)" },
  { key: "approver_role_ids", label: "Approver role IDs", help: "Comma-separated role IDs allowed to approve tool use" },
  { key: "otel_endpoint", label: "OTel endpoint", help: "OpenTelemetry OTLP/HTTP endpoint (blank = console telemetry)" },
  { key: "max_concurrent_runs", label: "Max concurrent runs", type: "int", default: "3", help: "Max simultaneous sessions" },
  {
    key: "approval_timeout_ms",
    label: "Approval timeout (ms)",
    type: "int",
    default: "300000",
    help: "How long to wait for a tool approval",
  },
  {
    key: "serve.enabled",
    label: "Serve enabled",
    type: "enum",
    oneOf: ["true", "false"],
    default: "false",
    help: "Expose the control plane over HTTP+WS for IDE/web clients",
  },
  {
    key: "serve.host",
    label: "Serve host",
    default: "127.0.0.1",
    help: "Bind address for the serve transport (127.0.0.1 = loopback only)",
  },
  { key: "serve.port", label: "Serve port", type: "int", default: "8787", help: "TCP port for the serve transport" },
  {
    key: "serve.auth_mode",
    label: "Serve auth mode",
    type: "enum",
    oneOf: ["loopback", "oidc"],
    default: "loopback",
    help: "loopback = local token; oidc = verify bearer JWT against an IdP",
  },
  { key: "oidc.issuer", label: "OIDC issuer", help: "OIDC issuer URL (discovery via <issuer>/.well-known/openid-configuration)" },
  { key: "oidc.audience", label: "OIDC audience", help: "Expected `aud` claim for access tokens" },
  { key: "oidc.jwks_uri", label: "OIDC JWKS URI", help: "Override JWKS URI (blank = resolve via issuer discovery)" },
  { key: "enabled_gateways", label: "Enabled gateways", control: true, help: "(managed by the Providers picker)" },
  { key: "enabled_runtimes", label: "Enabled runtimes", control: true, help: "(managed by the Providers picker)" },
  { key: "default_runtime", label: "Default runtime", control: true, help: "(managed by the Providers picker)" },
];

export function allFields(cat: ProviderCatalog = catalog): ConfigField[] {
  return [...GLOBAL_FIELDS, ...cat.gateways.flatMap((g) => g.fields), ...cat.runtimes.flatMap((r) => r.fields)];
}

export interface SettingDef {
  required?: boolean;
  secret?: boolean;
  default?: string;
  oneOf?: string[];
  int?: boolean;
}

export function fieldToDef(f: ConfigField): SettingDef {
  return {
    required: f.required,
    secret: f.secret,
    default: f.default,
    oneOf: f.type === "enum" ? f.oneOf : undefined,
    int: f.type === "int" ? true : undefined,
  };
}

export function buildSettingDefs(cat: ProviderCatalog = catalog): Record<string, SettingDef> {
  const defs: Record<string, SettingDef> = {};
  for (const f of allFields(cat)) defs[f.key] = fieldToDef(f);
  return defs;
}

export const SETTING_DEFS: Record<string, SettingDef> = buildSettingDefs(catalog);

export function validateSetting(key: string, value: string): string | null {
  const def = SETTING_DEFS[key];
  if (!def) return `unknown setting: ${key}`;
  if (def.oneOf && !def.oneOf.includes(value)) return `${key} must be one of: ${def.oneOf.join(", ")}`;
  if (def.int && !/^\d+$/.test(value)) return `${key} must be an integer`;
  return null;
}
