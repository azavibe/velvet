interface Token {
  value: string;
  start: number;
  end: number;
}

interface WakeMatch {
  consumed: number;
}

const OPTIONAL_PREFIXES = new Set([
  "agent", "hey", "hi", "hello", "okay", "ok", "bonjour", "salut", "hola",
  "ola", "oi", "привет", "здравствуите", "你好", "您好", "嗨", "こんにちは",
  "안녕", "안녕하세요",
]);

function normalizeWord(value: string): string {
  return value.normalize("NFKD").replace(/\p{M}/gu, "").toLocaleLowerCase();
}

function tokenize(text: string): Token[] {
  return Array.from(text.matchAll(/[\p{L}\p{N}]+/gu), (match) => ({
    value: normalizeWord(match[0]),
    start: match.index,
    end: match.index + match[0].length,
  }));
}

function levenshtein(a: string, b: string): number {
  const aa = Array.from(a);
  const bb = Array.from(b);
  let previous = Array.from({ length: bb.length + 1 }, (_, index) => index);
  for (let i = 1; i <= aa.length; i++) {
    const row = [i];
    for (let j = 1; j <= bb.length; j++) {
      row[j] = Math.min(
        previous[j] + 1,
        row[j - 1] + 1,
        previous[j - 1] + (aa[i - 1] === bb[j - 1] ? 0 : 1),
      );
    }
    previous = row;
  }
  return previous[bb.length];
}

function phoneticSkeleton(value: string): string {
  return normalizeWord(value)
    .replace(/^(?:th)/u, "t")
    .replace(/ph/gu, "f")
    .replace(/[ckq]/gu, "k")
    .replace(/[sz]/gu, "s")
    .replace(/[dt]/gu, "t")
    .replace(/[mn]/gu, "n")
    .replace(/[aeiouy]/gu, "")
    .replace(/(.)\1+/gu, "$1");
}

function fuzzyNameWord(actual: string, expected: string): boolean {
  if (actual === expected) return true;
  if (Array.from(actual).length < Math.max(3, Array.from(expected).length - 2)) return false;
  const budget = Array.from(expected).length >= 4 ? 2 : 1;
  if (levenshtein(actual, expected) <= budget) return true;
  const actualSkeleton = phoneticSkeleton(actual);
  const expectedSkeleton = phoneticSkeleton(expected);
  return actual[0] === expected[0]
    && Math.abs(actual.length - expected.length) <= 2
    && !!actualSkeleton
    && !!expectedSkeleton
    && levenshtein(actualSkeleton, expectedSkeleton) <= 1;
}

function wakeForms(agentName: string | null, aliases: string[]): string[][] {
  const seen = new Set<string>();
  return [agentName ?? "", ...aliases]
    .map((term) => tokenize(term).map((token) => token.value))
    .filter((words) => {
      const key = words.join(" ");
      if (!key || seen.has(key)) return false;
      seen.add(key);
      return true;
    })
    .sort((a, b) => b.length - a.length);
}

function matchWakeAt(tokens: Token[], offset: number, forms: string[][]): WakeMatch | null {
  let fuzzy: WakeMatch | null = null;
  for (const words of forms) {
    if (offset + words.length > tokens.length) continue;
    const actual = tokens.slice(offset, offset + words.length).map((token) => token.value);
    if (words.every((word, index) => actual[index] === word)) {
      return { consumed: words.length };
    }
    if (!fuzzy && words.every((word, index) => fuzzyNameWord(actual[index], word))) {
      fuzzy = { consumed: words.length };
    }
  }
  return fuzzy;
}

function collapseLeadingEchoes(text: string, forms: string[][]): string {
  const tokens = tokenize(text);
  if (!tokens.length) return text.trim();
  let offset = 0;
  while (offset < Math.min(tokens.length, 2) && OPTIONAL_PREFIXES.has(tokens[offset].value)) {
    offset++;
  }
  const first = matchWakeAt(tokens, offset, forms);
  if (!first) return text.trim();
  let cursor = offset + first.consumed;
  let repeats = 1;
  while (repeats < 4) {
    const next = matchWakeAt(tokens, cursor, forms);
    if (!next) break;
    cursor += next.consumed;
    repeats++;
  }
  if (repeats === 1) return text.trim();

  const firstEnd = tokens[offset + first.consumed - 1].end;
  const repeatedEnd = tokens[cursor - 1].end;
  const bodyStart = tokens[cursor]?.start ?? text.length;
  const separator = text.slice(repeatedEnd, bodyStart);
  return `${text.slice(0, firstEnd)}${separator}${text.slice(bodyStart)}`.trim();
}

function findTrailingWake(tokens: Token[], end: number, forms: string[][]): {
  start: number;
  match: WakeMatch;
} | null {
  const maxWords = Math.max(1, ...forms.map((form) => form.length));
  for (let start = Math.max(0, end - maxWords); start < end; start++) {
    const match = matchWakeAt(tokens, start, forms);
    if (match && start + match.consumed === end) return { start, match };
  }
  return null;
}

function collapseTrailingEchoes(text: string, forms: string[][]): string {
  const tokens = tokenize(text);
  if (tokens.length < 2) return text.trim();
  let cursor = tokens.length;
  const occurrences: Array<{ start: number; end: number }> = [];
  while (occurrences.length < 4) {
    const trailing = findTrailingWake(tokens, cursor, forms);
    if (!trailing) break;
    occurrences.unshift({ start: trailing.start, end: cursor });
    cursor = trailing.start;
  }
  if (!occurrences.length || cursor === 0) return text.trim();

  if (occurrences.length >= 2) {
    const first = occurrences[0];
    const firstEnd = tokens[first.end - 1].end;
    const finalEnd = tokens[occurrences[occurrences.length - 1].end - 1].end;
    return `${text.slice(0, firstEnd)}${text.slice(finalEnd)}`.trim();
  }

  // A lone wake term is ambiguous and normally preserved. Remove it only
  // when it follows an already-complete sentence, the characteristic shape
  // of a prompt-conditioned trailing echo.
  const occurrence = occurrences[0];
  const previousEnd = tokens[occurrence.start - 1].end;
  const boundary = text.slice(previousEnd, tokens[occurrence.start].start);
  if (/[.!?。！？]\s*$/u.test(boundary)) {
    return text.slice(0, tokens[occurrence.start].start).trimEnd();
  }
  return text.trim();
}

/** Remove only configured-name echoes at utterance boundaries. */
export function sanitizeAgentWakeEcho(
  text: string,
  agentName: string | null,
  aliases: string[] = [],
): string {
  const forms = wakeForms(agentName, aliases);
  if (!text.trim() || !forms.length) return text.trim();
  return collapseTrailingEchoes(collapseLeadingEchoes(text, forms), forms);
}
