// Provider Models card: batch-test plumbing (design:
// docs/design/2026-07-08-cockpit-ui-polish-batch-design.md §5).

export type ModelTestStatus = "valid" | "invalid" | "unknown";
export type ModelTestEntry = { status: ModelTestStatus; message: string };

/** Run `worker` over `items` with at most `limit` in flight (each model test
 * is a real billed inference call). Results keep item order. */
export async function runPool<T, R>(items: T[], limit: number, worker: (item: T) => Promise<R>): Promise<R[]> {
  const results = new Array<R>(items.length);
  let next = 0;
  const lanes = Array.from({ length: Math.max(1, Math.min(limit, items.length)) }, async () => {
    while (next < items.length) {
      const idx = next;
      next += 1;
      results[idx] = await worker(items[idx]);
    }
  });
  await Promise.all(lanes);
  return results;
}

/** Hide-invalid filter: drops only rows with a persisted "invalid" verdict;
 * untested, valid, and unknown rows always stay visible. */
export function visibleModels(models: string[], statuses: Map<string, ModelTestEntry>, hideInvalid: boolean): string[] {
  if (!hideInvalid) return models;
  return models.filter((model) => statuses.get(model)?.status !== "invalid");
}
