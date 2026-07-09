import type { CatalogEntry, ConnectionInfo } from "../bindings";
import type { ComboboxGroup, ComboboxOption } from "@ryuzi/ui";

/** Codex effort/review picker variants ("…-high", "…-review") share the base
 *  model's probe verdict. Mirror of `codex_base_model`
 *  (crates/core/src/llm_router/codex.rs): strip one trailing "-review", then
 *  one effort suffix — "xhigh" checked before "high" so "-xhigh" strips
 *  whole. */
function stripCodexVariant(model: string): string {
  let base = model.endsWith("-review") ? model.slice(0, -"-review".length) : model;
  for (const effort of ["xhigh", "high", "medium", "low", "none"]) {
    const suffix = `-${effort}`;
    if (base.endsWith(suffix)) {
      base = base.slice(0, -suffix.length);
      break;
    }
  }
  return base;
}

/** Normalize a picker value to the `(family, bare model)` pair that
 *  `model_status` verdicts are keyed by. Handles `family::model`
 *  route-target keys, `family/model` runtime ids, and `entry-id/model`
 *  endpoint-card ids (resolved to the entry's family via the catalog).
 *  Bare values are route aliases and unknown prefixes are unmappable —
 *  both return null so callers never filter them. */
export function modelStatusKey(value: string, catalog: CatalogEntry[]): { family: string; model: string } | null {
  const sep = value.indexOf("::");
  if (sep > 0) return { family: value.slice(0, sep), model: stripCodexVariant(value.slice(sep + 2)) };
  const slash = value.indexOf("/");
  if (slash <= 0) return null;
  const prefix = value.slice(0, slash);
  const entry = catalog.find((e) => e.family === prefix) ?? catalog.find((e) => e.id === prefix);
  if (!entry) return null;
  return { family: entry.family, model: stripCodexVariant(value.slice(slash + 1)) };
}

/** Group the composer's runtime model ids by provider family (PR #70 data).
 *  Runtime ids may be prefixed ("anthropic/claude-fable-5", or a catalog
 *  entry id like "anthropic-oauth/…" which resolves to its family): a known
 *  prefix wins and the label is trimmed to the part after it. Bare ids fall
 *  back to connection/catalog model lists, where only families with an
 *  ENABLED connection contribute. Unmatched BARE ids are route aliases (the
 *  backend emits routes as the only bare ids) and land in "Route", pinned
 *  first; unmatched PREFIXED ids land in "Other", pinned last. No usable
 *  data → flat list unchanged. Values are always the raw runtime ids. */
export function groupModelOptions(
  models: string[],
  catalog: CatalogEntry[],
  connections: ConnectionInfo[],
): ComboboxOption[] | ComboboxGroup[] {
  const opt = (m: string, label = m): ComboboxOption => ({ value: m, label, mono: true });
  if (models.length === 0 || catalog.length === 0) return models.map((m) => opt(m));

  const entryById = new Map(catalog.map((e) => [e.id, e]));
  const headByFamily = new Map(catalog.filter((e) => e.id === e.family).map((e) => [e.family, e]));
  const knownFamilies = new Set(catalog.map((e) => e.family));

  // Families with at least one ENABLED connection.
  const connectedFamilies = new Set<string>();
  for (const c of connections) {
    if (!c.enabled) continue;
    const entry = entryById.get(c.provider);
    if (entry) connectedFamilies.add(entry.family);
  }

  // model id → family: connection model lists first, then every catalog
  // entry belonging to a connected family (the family head's list covers
  // models an individual account doesn't enumerate).
  const familyByModel = new Map<string, string>();
  for (const c of connections) {
    if (!c.enabled) continue;
    const entry = entryById.get(c.provider);
    if (!entry) continue;
    for (const m of c.models) {
      if (!familyByModel.has(m)) familyByModel.set(m, entry.family);
    }
  }
  for (const e of catalog) {
    if (!connectedFamilies.has(e.family)) continue;
    for (const m of e.models) {
      if (!familyByModel.has(m)) familyByModel.set(m, e.family);
    }
  }

  const byFamily = new Map<string, ComboboxOption[]>();
  const route: ComboboxOption[] = [];
  const other: ComboboxOption[] = [];
  for (const m of models) {
    // Prefixed runtime id ("anthropic/claude-fable-5"): the prefix path
    // trusts the runtime list (it only contains connected providers' models
    // by construction), so the connected-families gate applies only to the
    // bare-id fallback below. A prefix that is a catalog ENTRY id rather
    // than a family ("anthropic-oauth/…", as built by the runtime detail
    // view's endpoint card) resolves to that entry's family. The value
    // stays the full raw id — only the label is trimmed.
    const slash = m.indexOf("/");
    const prefix = slash > 0 ? m.slice(0, slash) : null;
    if (prefix !== null) {
      const family = knownFamilies.has(prefix) ? prefix : entryById.get(prefix)?.family;
      if (family === undefined) {
        other.push(opt(m));
        continue;
      }
      const list = byFamily.get(family) ?? [];
      list.push(opt(m, m.slice(slash + 1)));
      byFamily.set(family, list);
      continue;
    }
    const family = familyByModel.get(m);
    if (family === undefined) {
      // Bare ids the connections/catalog don't know are route aliases: the
      // backend (selectable_native_models) emits routes as the only bare ids.
      route.push(opt(m));
      continue;
    }
    const list = byFamily.get(family) ?? [];
    list.push(opt(m));
    byFamily.set(family, list);
  }
  if (byFamily.size === 0) return models.map((m) => opt(m));

  const groups: ComboboxGroup[] = Array.from(byFamily, ([family, options]) => ({
    label: headByFamily.get(family)?.name ?? family,
    options,
  }));
  if (route.length > 0) groups.unshift({ label: "Route", options: route });
  if (other.length > 0) groups.push({ label: "Other", options: other });
  return groups;
}

/** Prepend a sentinel option (e.g. "Router default…", the combo item) ahead
 *  of a picker's option list. `Combobox` accepts only homogeneous arrays, so
 *  ahead of a grouped list the sentinel is wrapped as a headingless
 *  one-option group (an empty group label renders no header row). */
export function withLeadingOption(
  leading: ComboboxOption,
  options: ComboboxOption[] | ComboboxGroup[],
): ComboboxOption[] | ComboboxGroup[] {
  return isGrouped(options) ? [{ label: "", options: [leading] }, ...options] : [leading, ...options];
}

function isGrouped(options: ComboboxOption[] | ComboboxGroup[]): options is ComboboxGroup[] {
  const first = options[0];
  return first !== undefined && "options" in first;
}
