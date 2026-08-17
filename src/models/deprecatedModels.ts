/**
 * Models that have been shut down by their provider, mapped to the
 * replacement the provider recommends.
 *
 * A stored setting pointing at one of these isn't a cosmetic problem — the
 * API rejects the request, so enhancement or conversation suggestions just
 * fail with an opaque error until the user happens to open Settings and
 * pick something else. The migration in useSettings remaps them on load.
 *
 * Groq's replacements are the ones named in its own deprecation notices
 * (console.groq.com/docs/deprecations).
 */
export const DEPRECATED_MODEL_REPLACEMENTS: Record<string, string> = {
  // Shut down 2026-08-16
  "llama-3.1-8b-instant": "openai/gpt-oss-20b",
  "llama-3.3-70b-versatile": "openai/gpt-oss-120b",
  // Shut down 2026-07-17
  "qwen/qwen3-32b": "openai/gpt-oss-120b",
  "meta-llama/llama-4-scout-17b-16e-instruct": "openai/gpt-oss-120b",
  // Shut down 2026-04-15
  "moonshotai/kimi-k2-instruct-0905": "openai/gpt-oss-120b",
  // Shut down 2026-03-09
  "meta-llama/llama-4-maverick-17b-128e-instruct": "openai/gpt-oss-120b",
  // Shut down 2026-03-05
  "meta-llama/llama-guard-4-12b": "openai/gpt-oss-safeguard-20b",
};

/** The replacement for `modelId`, or `modelId` itself when it's still live. */
export function replaceIfDeprecated(modelId: string): string {
  return DEPRECATED_MODEL_REPLACEMENTS[modelId] ?? modelId;
}
