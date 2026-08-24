/**
 * Development-only startup marks. Keep details to timings and counts so
 * transcripts, settings, and filesystem paths never enter the console.
 */
export function startupMark(
  label: string,
  details?: Record<string, number | string | boolean>,
): void {
  if (!import.meta.env.DEV) return;

  const elapsedMs = Math.round(performance.now());
  if (details) {
    console.debug(`[startup +${elapsedMs}ms] ${label}`, details);
  } else {
    console.debug(`[startup +${elapsedMs}ms] ${label}`);
  }
}
