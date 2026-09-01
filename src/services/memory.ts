import {
  getApiKey,
  processReasoning,
  storeMemoryCandidates,
  type MemoryCandidate,
} from "@/services/tauriApi";

export type MemorySourceType = "dictation" | "conversation" | "note";

export interface MemoryExtractionSettings {
  automaticMemoryEnabled: boolean | null;
  reasoningModel: string | null;
  reasoningProvider: string | null;
}

const MAX_SOURCE_CHARS = 4_000;
const MAX_CANDIDATES = 20;
const MIN_CONFIDENCE = 0.6;
const VALID_KINDS = new Set(["entity", "fact", "relationship", "summary"]);

function optionalText(value: unknown, max: number): string | null {
  if (value === null || value === undefined) return null;
  if (typeof value !== "string") return null;
  const text = value.trim();
  return text && text.length <= max ? text : null;
}

export function parseMemoryCandidates(response: string): MemoryCandidate[] {
  const unfenced = response.replace(/^```(?:json)?\s*/i, "").replace(/\s*```$/i, "").trim();
  const start = unfenced.indexOf("{");
  const end = unfenced.lastIndexOf("}");
  if (start < 0 || end <= start) return [];

  try {
    const parsed = JSON.parse(unfenced.slice(start, end + 1)) as Record<string, unknown>;
    if (!Array.isArray(parsed.memories)) return [];
    return parsed.memories.slice(0, MAX_CANDIDATES).flatMap((raw) => {
      if (!raw || typeof raw !== "object") return [];
      const item = raw as Record<string, unknown>;
      const kind = typeof item.kind === "string" ? item.kind : "";
      const canonical = optionalText(item.canonical_text, 500);
      const confidence = item.confidence;
      if (
        !VALID_KINDS.has(kind)
        || !canonical
        || typeof confidence !== "number"
        || !Number.isFinite(confidence)
        || confidence < MIN_CONFIDENCE
        || confidence > 1
      ) return [];

      const aliases = Array.isArray(item.aliases)
        ? item.aliases
          .map((alias) => optionalText(alias, 120))
          .filter((alias): alias is string => alias !== null)
          .slice(0, 12)
        : [];
      return [{
        kind: kind as MemoryCandidate["kind"],
        canonical_text: canonical,
        subject: optionalText(item.subject, 160),
        predicate: optionalText(item.predicate, 120),
        object: optionalText(item.object, 300),
        aliases,
        confidence,
      }];
    });
  } catch {
    return [];
  }
}

export function memoryExtractionPrompts(text: string): {
  systemPrompt: string;
  userPrompt: string;
} {
  const systemPrompt = [
    "Extract only durable, explicitly stated information from the supplied speech.",
    "Keep stable people, organizations, product names, user preferences, and relationships.",
    "Do not infer, speculate, preserve secrets, or store transient requests, schedules, tasks, greetings, or conversation filler.",
    "Use a short normalized canonical_text. For facts and relationships, also supply stable subject, predicate, and object strings.",
    "Aliases must only be spelling or naming variants explicitly supported by the speech.",
    "Return JSON only: {\"memories\":[{\"kind\":\"entity|fact|relationship|summary\",\"canonical_text\":string,\"subject\":string|null,\"predicate\":string|null,\"object\":string|null,\"aliases\":string[],\"confidence\":number}]}",
    "Return at most 20 memories. Confidence must be between 0 and 1; omit anything below 0.6.",
  ].join(" ");
  return {
    systemPrompt,
    userPrompt: JSON.stringify({ speech: text.trim().slice(0, MAX_SOURCE_CHARS) }),
  };
}

export async function extractAndStoreMemory(
  text: string,
  sourceType: MemorySourceType,
  sourceId: number,
  settings: MemoryExtractionSettings,
  timeoutMs = 8_000,
): Promise<number> {
  if (
    !settings.automaticMemoryEnabled
    || !text.trim()
    || sourceId <= 0
    || !settings.reasoningProvider
    || !settings.reasoningModel
  ) return 0;

  const apiKey = await getApiKey(settings.reasoningProvider);
  if (!apiKey) return 0;
  const prompts = memoryExtractionPrompts(text);
  let timeoutId: ReturnType<typeof globalThis.setTimeout> | undefined;
  try {
    const response = await Promise.race([
      processReasoning(
        prompts.userPrompt,
        settings.reasoningModel,
        settings.reasoningProvider,
        prompts.systemPrompt,
        apiKey,
        800,
        0,
      ),
      new Promise<never>((_, reject) => {
        timeoutId = globalThis.setTimeout(
          () => reject(new Error("memory_extraction_timeout")),
          timeoutMs,
        );
      }),
    ]);
    const candidates = parseMemoryCandidates(response);
    if (candidates.length === 0) return 0;
    return storeMemoryCandidates(sourceType, sourceId, candidates);
  } catch {
    return 0;
  } finally {
    if (timeoutId !== undefined) globalThis.clearTimeout(timeoutId);
  }
}
