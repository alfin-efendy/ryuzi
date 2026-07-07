import type { CatalogEntry, ConnectionInfo } from "../bindings";
import type { ComboboxGroup, ComboboxOption } from "@ryuzi/ui";

/** Group the composer's runtime model ids by provider family (PR #70 data).
 *  Runtime ids may be family-prefixed ("anthropic/claude-fable-5"): a known
 *  prefix wins and the label is trimmed to the part after it. Bare ids fall
 *  back to connection/catalog model lists, where only families with an
 *  ENABLED connection contribute. Unmatched ids land in "Other". No usable
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
  const other: ComboboxOption[] = [];
  for (const m of models) {
    // Family-prefixed runtime id ("anthropic/claude-fable-5"): the prefix path
    // trusts the runtime list (it only contains connected providers' models by
    // construction), so the connected-families gate applies only to the
    // bare-id fallback below. The value stays the full raw id — only the
    // label is trimmed.
    const slash = m.indexOf("/");
    const prefix = slash > 0 ? m.slice(0, slash) : null;
    if (prefix !== null && knownFamilies.has(prefix)) {
      const list = byFamily.get(prefix) ?? [];
      list.push(opt(m, m.slice(slash + 1)));
      byFamily.set(prefix, list);
      continue;
    }
    const family = familyByModel.get(m);
    if (family === undefined) {
      other.push(opt(m));
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
  if (other.length > 0) groups.push({ label: "Other", options: other });
  return groups;
}
