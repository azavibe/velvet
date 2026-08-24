// Increment when release notes are materially revised without changing the
// application version. The composite key lets startup and Settings agree on
// whether the current notes have been presented.
export const WHATS_NEW_REVISION = "2026-08-24";

export function whatsNewReleaseKey(version: string): string {
  return `${version}:${WHATS_NEW_REVISION}`;
}
