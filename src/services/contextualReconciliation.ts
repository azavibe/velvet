import type { DictionaryEntry } from "@/models/dictionary";
import type { MemoryContextItem } from "@/services/tauriApi";

export type ReconciliationStatus =
  | "disabled"
  | "unchanged"
  | "accepted"
  | "uncertain"
  | "failed";

export interface ReconciliationContext {
  dictionary: DictionaryEntry[];
  agentName: string;
  agentAliases: string[];
  recentTexts: string[];
  knownMemories?: MemoryContextItem[];
  language?: string | null;
}

export interface ReconciliationResult {
  text: string;
  status: ReconciliationStatus;
  confidence: number | null;
  evidence: "dictionary" | "agent" | "recent" | "memory" | null;
}

export type ReconciliationModelCall = (
  systemPrompt: string,
  userPrompt: string,
) => Promise<string>;

const MIN_CONFIDENCE = 0.9;
const MAX_CONTEXT_ITEMS = 8;
const MAX_CONTEXT_ITEM_CHARS = 240;
const MAX_TOTAL_CONTEXT_CHARS = 1200;

function normalized(value: string): string {
  return value.normalize("NFKC").toLocaleLowerCase();
}

function comparable(value: string): string {
  return normalized(value).replace(/[^\p{L}\p{N}]+/gu, "");
}

function containsPhrase(text: string, phrase: string): boolean {
  const target = normalized(phrase.trim());
  if (!target) return false;
  const source = normalized(text);
  if (/\p{Script=Han}|\p{Script=Hiragana}|\p{Script=Katakana}|\p{Script=Hangul}/u.test(target)) {
    return source.includes(target);
  }
  const escaped = target.replace(/[.*+?^${}()|[\]\\]/g, "\\$&");
  return new RegExp(
    `(^|[^\\p{L}\\p{N}])${escaped}($|[^\\p{L}\\p{N}])`,
    "u",
  ).test(source);
}

function editDistance(left: string, right: string): number {
  if (left === right) return 0;
  if (!left) return right.length;
  if (!right) return left.length;
  let previous = Array.from({ length: right.length + 1 }, (_, index) => index);
  for (let i = 1; i <= left.length; i += 1) {
    const current = [i];
    for (let j = 1; j <= right.length; j += 1) {
      current[j] = Math.min(
        current[j - 1] + 1,
        previous[j] + 1,
        previous[j - 1] + (left[i - 1] === right[j - 1] ? 0 : 1),
      );
    }
    previous = current;
  }
  return previous[right.length];
}

function isSmallCorrection(rawText: string, candidate: string): boolean {
  const raw = comparable(rawText);
  const corrected = comparable(candidate);
  if (!raw || !corrected) return false;
  const lengthRatio = corrected.length / raw.length;
  if (lengthRatio < 0.7 || lengthRatio > 1.3) return false;
  return editDistance(raw, corrected) <= Math.max(3, Math.ceil(raw.length * 0.22));
}

function dictionaryEvidence(
  rawText: string,
  candidate: string,
  dictionary: DictionaryEntry[],
): boolean {
  return dictionary.some((entry) =>
    entry.aliases.some(
      (alias) => containsPhrase(rawText, alias) && containsPhrase(candidate, entry.term),
    ),
  );
}

function agentEvidence(
  rawText: string,
  candidate: string,
  agentName: string,
  aliases: string[],
): boolean {
  return [agentName, ...aliases].some((term) => {
    const canonical = comparable(term);
    if (canonical.length < 3 || !containsPhrase(candidate, term)) return false;
    return normalized(rawText)
      .split(/[^\p{L}\p{N}]+/u)
      .filter(Boolean)
      .some((word) => {
        const value = comparable(word);
        return value !== canonical
          && editDistance(value, canonical) <= Math.max(1, Math.ceil(canonical.length * 0.25));
      });
  });
}

function words(value: string): string[] {
  return normalized(value).match(/[\p{L}\p{N}]+/gu) ?? [];
}

function ngrams(value: string): string[] {
  const tokens = words(value);
  const result: string[] = [];
  for (let width = 1; width <= 3; width += 1) {
    for (let index = 0; index + width <= tokens.length; index += 1) {
      const phrase = tokens.slice(index, index + width).join("");
      if (phrase.length >= 4) result.push(phrase);
    }
  }
  return result;
}

function recentEvidence(rawText: string, candidate: string, recentTexts: string[]): boolean {
  const rawPhrases = ngrams(rawText);
  const candidatePhrases = ngrams(candidate).filter(
    (phrase) => !rawPhrases.includes(phrase),
  );
  const recent = comparable(recentTexts.join(" "));
  return candidatePhrases.some((candidatePhrase) => {
    if (!recent.includes(candidatePhrase)) return false;
    return rawPhrases.some((rawPhrase) => {
      const maxLength = Math.max(rawPhrase.length, candidatePhrase.length);
      return editDistance(rawPhrase, candidatePhrase)
        <= Math.max(2, Math.ceil(maxLength * 0.35));
    });
  });
}

function memoryEvidence(
  rawText: string,
  candidate: string,
  memories: MemoryContextItem[],
): boolean {
  return memories.some((memory) =>
    recentEvidence(rawText, candidate, [memory.canonical_text, ...memory.aliases]),
  );
}

function validateEvidence(
  rawText: string,
  candidate: string,
  context: ReconciliationContext,
): ReconciliationResult["evidence"] {
  if (dictionaryEvidence(rawText, candidate, context.dictionary)) return "dictionary";
  if (agentEvidence(rawText, candidate, context.agentName, context.agentAliases)) return "agent";
  if (memoryEvidence(rawText, candidate, context.knownMemories ?? [])) return "memory";
  if (recentEvidence(rawText, candidate, context.recentTexts)) return "recent";
  return null;
}

function boundedRecentTexts(values: string[]): string[] {
  const result: string[] = [];
  let total = 0;
  for (const value of values.slice(0, MAX_CONTEXT_ITEMS)) {
    const text = value.trim().slice(0, MAX_CONTEXT_ITEM_CHARS);
    if (!text || total + text.length > MAX_TOTAL_CONTEXT_CHARS) break;
    result.push(text);
    total += text.length;
  }
  return result;
}

export function reconciliationPrompts(
  rawText: string,
  context: ReconciliationContext,
): { systemPrompt: string; userPrompt: string } {
  const recentTexts = boundedRecentTexts(context.recentTexts);
  const dictionary = context.dictionary
    .filter((entry) => entry.term.trim())
    .slice(0, 40)
    .map((entry, index) => ({
      id: `D${index + 1}`,
      term: entry.term.slice(0, 100),
      aliases: entry.aliases.slice(0, 8).map((alias) => alias.slice(0, 100)),
    }));
  const knownMemories = (context.knownMemories ?? [])
    .slice(0, 12)
    .map((memory, index) => ({
      id: `M${index + 1}`,
      text: memory.canonical_text.slice(0, 240),
      aliases: memory.aliases.slice(0, 8).map((alias) => alias.slice(0, 100)),
    }));
  const systemPrompt = [
    "You are a conservative speech-recognition reconciler.",
    "Correct only likely ASR mistakes supported by the supplied dictionary, agent identity, confirmed memory, or recent text.",
    "Do not rewrite grammar, punctuation, tone, meaning, or formatting. Do not add facts.",
    "If evidence is weak or ambiguous, return the input unchanged.",
    "Return JSON only: {\"text\":string,\"confidence\":number}. Confidence must be between 0 and 1.",
  ].join(" ");
  const userPrompt = JSON.stringify({
    raw_text: rawText,
    language: context.language ?? null,
    agent: [context.agentName, ...context.agentAliases].filter(Boolean).slice(0, 12),
    dictionary,
    confirmed_memory: knownMemories,
    recent_text: recentTexts.map((text, index) => ({ id: `R${index + 1}`, text })),
  });
  return { systemPrompt, userPrompt };
}

function parseCandidate(response: string): { text: string; confidence: number } | null {
  const unfenced = response.replace(/^```(?:json)?\s*/i, "").replace(/\s*```$/i, "").trim();
  const start = unfenced.indexOf("{");
  const end = unfenced.lastIndexOf("}");
  if (start < 0 || end <= start) return null;
  try {
    const value = JSON.parse(unfenced.slice(start, end + 1)) as Record<string, unknown>;
    if (typeof value.text !== "string" || typeof value.confidence !== "number") return null;
    if (
      !Number.isFinite(value.confidence)
      || value.confidence < 0
      || value.confidence > 1
    ) return null;
    return { text: value.text.trim(), confidence: value.confidence };
  } catch {
    return null;
  }
}

export async function reconcileSpeech(
  rawText: string,
  context: ReconciliationContext,
  callModel: ReconciliationModelCall,
  timeoutMs = 5000,
): Promise<ReconciliationResult> {
  const unchanged: ReconciliationResult = {
    text: rawText,
    status: "unchanged",
    confidence: null,
    evidence: null,
  };
  if (!rawText.trim()) return unchanged;

  const prompts = reconciliationPrompts(rawText, context);
  let timeoutId: ReturnType<typeof globalThis.setTimeout> | undefined;
  try {
    const response = await Promise.race([
      callModel(prompts.systemPrompt, prompts.userPrompt),
      new Promise<never>((_, reject) => {
        timeoutId = globalThis.setTimeout(
          () => reject(new Error("reconciliation_timeout")),
          timeoutMs,
        );
      }),
    ]);
    const parsed = parseCandidate(response);
    if (!parsed) return { ...unchanged, status: "failed" };
    if (parsed.text === rawText) {
      return { ...unchanged, confidence: parsed.confidence };
    }
    if (
      parsed.confidence < MIN_CONFIDENCE
      || !isSmallCorrection(rawText, parsed.text)
    ) {
      return { ...unchanged, status: "uncertain", confidence: parsed.confidence };
    }
    const evidence = validateEvidence(rawText, parsed.text, context);
    if (!evidence) {
      return { ...unchanged, status: "uncertain", confidence: parsed.confidence };
    }
    return {
      text: parsed.text,
      status: "accepted",
      confidence: parsed.confidence,
      evidence,
    };
  } catch {
    return { ...unchanged, status: "failed" };
  } finally {
    if (timeoutId !== undefined) globalThis.clearTimeout(timeoutId);
  }
}
