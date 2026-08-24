export function mergeUniqueById<T extends { id: number }>(previous: T[], incoming: T[]): T[] {
  const result: T[] = [];
  const seen = new Set<number>();
  for (const item of [...previous, ...incoming]) {
    if (seen.has(item.id)) continue;
    seen.add(item.id);
    result.push(item);
  }
  return result;
}

/** Keep one rendered source card even if a refresh or overlapping page query
 * hands React the same source more than once. */
export function uniqueHistoryItems<T extends { id: string }>(items: T[]): T[] {
  const result: T[] = [];
  const seen = new Set<string>();
  for (const item of items) {
    if (seen.has(item.id)) continue;
    seen.add(item.id);
    result.push(item);
  }
  return result;
}
