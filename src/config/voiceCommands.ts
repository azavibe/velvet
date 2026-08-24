/** Bounded, whole-utterance application voice commands. */

export type VoiceCommand =
  | { kind: "start-note" }
  | { kind: "start-conversation"; personaId: string | null }
  | { kind: "stop" }
  | { kind: "open-settings" };

export type VoiceCommandActionId = VoiceCommand["kind"];
export type VoiceCommandRisk = "low" | "restricted";
export type VoiceCommandMatchType = "exact" | "fuzzy";
export type VoiceCommandWakeMatch = "none" | "exact" | "fuzzy";

export interface VoiceCommandDefinition {
  id: VoiceCommandActionId;
  phrases: readonly string[];
  risk: VoiceCommandRisk;
  confirmationRequired: boolean;
}

/** The command registry is the vocabulary source for matching and ASR retry prompts. */
export const VOICE_COMMAND_REGISTRY: readonly VoiceCommandDefinition[] = [
  {
    id: "start-note",
    phrases: [
      "note", "notes", "start note", "start notes", "take a note", "take notes",
      "begin note", "begin notes", "new note", "new notes", "create note",
      "create notes", "make note", "make notes", "open note", "open notes",
      "note taking", "notes mode",
    ],
    risk: "low",
    confirmationRequired: false,
  },
  {
    id: "start-conversation",
    phrases: [
      "conversation", "conversations", "call", "start call", "begin call", "chat",
      "start conversation", "start a conversation", "begin conversation",
      "begin a conversation", "new conversation", "new chat", "open conversation",
    ],
    risk: "low",
    confirmationRequired: false,
  },
  {
    id: "open-settings",
    phrases: ["settings", "open settings", "preferences", "open preferences"],
    risk: "low",
    confirmationRequired: false,
  },
  {
    id: "stop",
    phrases: [
      "stop", "stop it", "stop that", "stop this", "stop recording", "stop notes",
      "stop note", "stop conversation", "stop meeting", "stop call", "end conversation",
      "end the conversation", "end call",
      "finish note", "finish notes", "cancel recording",
    ],
    risk: "restricted",
    confirmationRequired: false,
  },
] as const;

export interface VoiceCommandPersona { id: string; name: string }
export interface VoiceCommandMatch { command: VoiceCommand; matchType: VoiceCommandMatchType }
export type VoiceCommandRejectionReason =
  | "no_configured_wake_name"
  | "wake_name_not_found"
  | "bare_wake_name"
  | "command_too_long"
  | "conversational_question"
  | "unsupported_command";
export interface VoiceCommandDiagnostics {
  candidate: boolean;
  wakeMatch: VoiceCommandWakeMatch;
  matchType: VoiceCommandMatchType | null;
  action: VoiceCommandActionId | null;
  rejectionReason: VoiceCommandRejectionReason | null;
}
export interface VoiceCommandDetection {
  match: VoiceCommandMatch | null;
  diagnostics: VoiceCommandDiagnostics;
}

const GREETINGS = new Set([
  "hey", "hi", "hello", "ok", "okay", "hallo", "bonjour", "salut", "hola", "olá", "oi",
  "привет", "здравствуйте", "你好", "您好", "嗨", "こんにちは", "안녕", "안녕하세요",
]);
const OPTIONAL_WAKE_PREFIXES = new Set(["agent"]);
const LEADING_FILLERS = new Set(["please", "kindly", "just"]);
const QUESTION_STARTERS = new Set([
  "what", "who", "why", "when", "where", "how", "which", "whose", "can", "could", "would",
]);
const MAX_COMMAND_TOKENS = 8;
const MAX_NAME_REPEATS = 3;

function normalizeWord(value: string): string {
  return value.normalize("NFKD").replace(/\p{M}/gu, "").toLocaleLowerCase();
}

function tokenize(text: string): string[] {
  return Array.from(text.normalize("NFKC").matchAll(/[\p{L}\p{N}]+/gu), (match) =>
    normalizeWord(match[0]),
  );
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

/** Small language-neutral consonant skeleton used only after whole-utterance gating. */
function phoneticSkeleton(value: string): string {
  const normalized = normalizeWord(value)
    .replace(/^(?:th)/u, "t")
    .replace(/ph/gu, "f")
    .replace(/[ckq]/gu, "k")
    .replace(/[sz]/gu, "s")
    .replace(/[dt]/gu, "t")
    .replace(/[mn]/gu, "n")
    .replace(/[aeiouy]/gu, "")
    .replace(/(.)\1+/gu, "$1");
  return normalized;
}

function tokenDistance(a: string, b: string): number {
  const literal = levenshtein(a, b);
  const pa = phoneticSkeleton(a);
  const pb = phoneticSkeleton(b);
  return Math.min(literal, pa && pb ? levenshtein(pa, pb) : literal);
}

function nameTokenMatches(token: string, expected: string): boolean {
  if (token === expected) return true;
  const budget = Array.from(expected).length >= 4 ? 2 : 1;
  if (levenshtein(token, expected) <= budget) return true;
  return token[0] === expected[0]
    && Math.abs(token.length - expected.length) <= 2
    && levenshtein(phoneticSkeleton(token), phoneticSkeleton(expected)) <= 1;
}

interface WakeResult {
  match: VoiceCommandWakeMatch;
  consumed: number;
}

function matchWakeAt(
  tokens: string[],
  offset: number,
  terms: string[],
  allowPhonetic: boolean,
): WakeResult | null {
  let bestFuzzy: WakeResult | null = null;
  for (const term of terms) {
    const words = tokenize(term);
    if (!words.length || offset + words.length > tokens.length) continue;
    if (words.every((word, index) => tokens[offset + index] === word)) {
      return { match: "exact", consumed: words.length };
    }
    if (words.every((word, index) => allowPhonetic
      ? nameTokenMatches(tokens[offset + index], word)
      : levenshtein(tokens[offset + index], word) <= (word.length >= 4 ? 2 : 1))) {
      bestFuzzy ??= { match: "fuzzy", consumed: words.length };
    }
  }
  return bestFuzzy;
}

function inspectWake(text: string, agentName: string | null, aliases?: string[]): {
  wakeMatch: VoiceCommandWakeMatch;
  remainder: string[];
} {
  const terms = [agentName ?? "", ...(aliases ?? [])].filter((term) => term.trim());
  if (!terms.length) return { wakeMatch: "none", remainder: [] };
  const tokens = tokenize(text);
  let index = GREETINGS.has(tokens[0]) ? 1 : 0;
  if (OPTIONAL_WAKE_PREFIXES.has(tokens[index])) index++;
  let wakeMatch: VoiceCommandWakeMatch = "none";
  for (let repeat = 0; repeat < MAX_NAME_REPEATS; repeat++) {
    const match = matchWakeAt(tokens, index, terms, repeat === 0);
    if (!match) break;
    index += match.consumed;
    wakeMatch = wakeMatch === "fuzzy" || match.match === "fuzzy" ? "fuzzy" : "exact";
  }
  return { wakeMatch, remainder: wakeMatch === "none" ? [] : tokens.slice(index) };
}

function normalizeRemainder(tokens: string[]): string[] {
  let result = [...tokens];
  while (LEADING_FILLERS.has(result[0])) result = result.slice(1);
  if ((result[0] === "could" || result[0] === "can") && result[1] === "you") result = result.slice(2);
  const endings: readonly (readonly string[])[] = [["please"], ["thanks"], ["thank", "you"]];
  let changed = true;
  while (changed) {
    changed = false;
    for (const ending of endings) {
      if (result.length >= ending.length && ending.every((word, i) => result[result.length - ending.length + i] === word)) {
        result = result.slice(0, -ending.length);
        changed = true;
      }
    }
  }
  return result;
}

function commandFor(id: VoiceCommandActionId): VoiceCommand {
  if (id === "start-note") return { kind: id };
  if (id === "start-conversation") return { kind: id, personaId: null };
  return { kind: id };
}

function personaPhrases(persona: VoiceCommandPersona): string[][] {
  const name = tokenize(persona.name);
  return [name, ["start", ...name], ["open", ...name], ["use", ...name], ["switch", "to", ...name]];
}

function exactAction(tokens: string[], personas: VoiceCommandPersona[]): VoiceCommand | null {
  const phrase = tokens.join(" ");
  for (const definition of VOICE_COMMAND_REGISTRY) {
    if (definition.phrases.some((candidate) => tokenize(candidate).join(" ") === phrase)) {
      return commandFor(definition.id);
    }
  }
  for (const persona of personas) {
    if (persona.name.trim() && personaPhrases(persona).some((candidate) => candidate.join(" ") === phrase)) {
      return { kind: "start-conversation", personaId: persona.id };
    }
  }
  return null;
}

/** A bare low-risk phrase may justify one audio retry, but is never executable
 * by itself. The retry must recover the configured wake and the same action. */
export function matchBareLowRiskRetryAction(
  text: string,
  personas: VoiceCommandPersona[],
): VoiceCommand | null {
  const body = normalizeRemainder(tokenize(text));
  if (!body.length || body.length > 4 || QUESTION_STARTERS.has(body[0])) return null;
  const command = exactAction(body, personas);
  return command?.kind === "stop" ? null : command;
}

function fuzzyTokenMatches(actual: string, expected: string, risk: VoiceCommandRisk): boolean {
  if (actual === expected) return true;
  if (risk === "restricted") return false;
  return tokenDistance(actual, expected) <= 1;
}

function fuzzyPhrase(tokens: string[], phrase: readonly string[], risk: VoiceCommandRisk): boolean {
  return tokens.length === phrase.length && phrase.every((word, index) =>
    fuzzyTokenMatches(tokens[index], word, risk),
  );
}

function fuzzyAction(
  tokens: string[],
  wakeMatch: VoiceCommandWakeMatch,
  personas: VoiceCommandPersona[],
): VoiceCommand | null {
  for (const definition of VOICE_COMMAND_REGISTRY) {
    for (const rawPhrase of definition.phrases) {
      const phrase = tokenize(rawPhrase);
      if (definition.id === "start-conversation" && phrase.some((word) => word === "call" || word === "chat")) {
        continue;
      }
      if (fuzzyPhrase(tokens, phrase, definition.risk) && tokens.join(" ") !== phrase.join(" ")) {
        return commandFor(definition.id);
      }
    }
  }
  // Observed Polish-shaped Whisper output for English "call". This is not a
  // replacement rule: it is eligible only after an exact wake and as the
  // complete command body.
  if (wakeMatch === "exact" && tokens.length === 1 && tokens[0] === "kolejny") {
    return { kind: "start-conversation", personaId: null };
  }
  for (const persona of personas) {
    if (persona.name.trim() && personaPhrases(persona).some((phrase) => fuzzyPhrase(tokens, phrase, "low"))) {
      return { kind: "start-conversation", personaId: persona.id };
    }
  }
  return null;
}

export function detectVoiceCommandWithDiagnostics(
  text: string,
  agentName: string | null,
  aliases: string[] | undefined,
  personas: VoiceCommandPersona[],
): VoiceCommandDetection {
  const configured = [agentName ?? "", ...(aliases ?? [])].some((term) => term.trim());
  const wake = inspectWake(text, agentName, aliases);
  const body = normalizeRemainder(wake.remainder);
  const candidate = wake.wakeMatch !== "none" && body.length > 0;
  let rejectionReason: VoiceCommandRejectionReason | null = null;
  let match: VoiceCommandMatch | null = null;

  if (!configured) rejectionReason = "no_configured_wake_name";
  else if (wake.wakeMatch === "none") rejectionReason = "wake_name_not_found";
  else if (!body.length) rejectionReason = "bare_wake_name";
  else if (body.length > MAX_COMMAND_TOKENS) rejectionReason = "command_too_long";
  else if (QUESTION_STARTERS.has(body[0])) rejectionReason = "conversational_question";
  else {
    const exact = exactAction(body, personas);
    if (exact) match = { command: exact, matchType: wake.wakeMatch === "exact" ? "exact" : "fuzzy" };
    else {
      const fuzzy = fuzzyAction(body, wake.wakeMatch, personas);
      if (fuzzy) match = { command: fuzzy, matchType: "fuzzy" };
      else rejectionReason = "unsupported_command";
    }
  }

  return {
    match,
    diagnostics: {
      candidate,
      wakeMatch: wake.wakeMatch,
      matchType: match?.matchType ?? null,
      action: match?.command.kind ?? null,
      rejectionReason: match ? null : rejectionReason,
    },
  };
}

export function detectVoiceCommand(
  text: string,
  agentName: string | null,
  aliases: string[] | undefined,
  personas: VoiceCommandPersona[],
): VoiceCommand | null {
  return detectVoiceCommandWithDiagnostics(text, agentName, aliases, personas).match?.command ?? null;
}

export function matchVoiceCommand(
  text: string,
  agentName: string | null,
  aliases: string[] | undefined,
  personas: VoiceCommandPersona[],
): VoiceCommandMatch | null {
  return detectVoiceCommandWithDiagnostics(text, agentName, aliases, personas).match;
}

/** Whether a failed local action match is plausible enough to spend one ASR retry. */
export function isCommandRetryCandidate(
  detection: VoiceCommandDetection,
  text: string,
  personas: VoiceCommandPersona[] = [],
): boolean {
  if (!detection.diagnostics.candidate || detection.diagnostics.wakeMatch === "none") return false;
  if (detection.diagnostics.rejectionReason !== "unsupported_command") return false;
  const tokens = tokenize(text);
  if (tokens.length > MAX_COMMAND_TOKENS + 3) return false;
  let wakeEnd = GREETINGS.has(tokens[0]) ? 2 : 1;
  if (OPTIONAL_WAKE_PREFIXES.has(tokens[0]) || OPTIONAL_WAKE_PREFIXES.has(tokens[1])) wakeEnd++;
  const body = normalizeRemainder(tokens.slice(wakeEnd));
  if (!body.length || QUESTION_STARTERS.has(body[0])) return false;
  const retryPhrases = [
    ...VOICE_COMMAND_REGISTRY.flatMap((entry) => entry.phrases.map((phrase) => tokenize(phrase))),
    ...personas.flatMap((persona) => personaPhrases(persona)),
  ];
  return retryPhrases.some((phrase) => {
    return body.length === phrase.length && phrase.every((expected, index) =>
      tokenDistance(body[index], expected) <= (expected.length >= 6 ? 3 : 2),
    );
  });
}

export function buildCommandRetryPrompt(
  agentName: string,
  aliases: string[],
  applicationLanguage?: string,
  personas: VoiceCommandPersona[] = [],
): string {
  const wakeTerms = [agentName, ...aliases].filter((term) => term.trim()).join(", ");
  const commandWords = ["notes", "conversation", "call", "support", "settings", "stop"];
  const personaNames = personas.map((persona) => persona.name.trim()).filter(Boolean);
  const vocabulary = [...new Set([...commandWords, ...personaNames])].join(", ");
  const languageHint = applicationLanguage?.trim() ? ` Application language: ${applicationLanguage}.` : "";
  return `Spelling context only: the proper wake name may be ${wakeTerms}; application vocabulary: ${vocabulary}.${languageHint} Preserve every spoken word, including a leading wake name. Do not add words that were not spoken.`;
}
