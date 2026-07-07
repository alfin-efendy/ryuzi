import type { CatalogEntry, ConnectionInfo } from "../bindings";
import type { ComboboxGroup, ComboboxOption } from "@ryuzi/ui";

/** Group the composer's runtime model ids by provider family (PR #70 data).
 *  Only families with an ENABLED connection contribute; a model claimed by a
 *  connection's model list or its catalog entry belongs to that family.
 *  Unmatched ids land in "Other". No usable data → flat list unchanged. */
export function groupModelOptions(
  models: string[],
  catalog: CatalogEntry[],
  connections: ConnectionInfo[],
): ComboboxOption[] | ComboboxGroup[] {
  const opt = (m: string): ComboboxOption => ({ value: m, label: m, mono: true });
  if (models.length === 0 || catalog.length === 0) return models.map(opt);

  const entryById = new Map(catalog.map((e) => [e.id, e]));
  const headByFamily = new Map(catalog.filter((e) => e.id === e.family).map((e) => [e.family, e]));

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
    const family = familyByModel.get(m);
    if (family === undefined) {
      other.push(opt(m));
      continue;
    }
    const list = byFamily.get(family) ?? [];
    list.push(opt(m));
    byFamily.set(family, list);
  }
  if (byFamily.size === 0) return models.map(opt);

  const groups: ComboboxGroup[] = Array.from(byFamily, ([family, options]) => ({
    label: headByFamily.get(family)?.name ?? family,
    options,
  }));
  if (other.length > 0) groups.push({ label: "Other", options: other });
  return groups;
}
