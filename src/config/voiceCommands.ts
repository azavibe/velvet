/**
 * Voice commands: spoken instructions that make the app *do* something
 * ("Aral, start notes") rather than dictate text or ask the agent a
 * question ("Aral, what's the capital of France?").
 *
 * Deliberately stricter than `detectChatMode` in prompts.ts: chat mode only
 * needs the utterance to *open* with an address to the agent, because
 * everything after it is the question. A command has to be the *entire*
 * utterance — "Aral, start notes" is a command, but "Aral, start notes with
 * the following headings…" is not, it's a request for the agent. That whole-
 * utterance rule is what keeps ordinary dictation from accidentally
 * triggering an action, and it's also what makes it safe to be forgiving
 * about the name itself (see `isNameToken`).
 */

export type VoiceCommand =
  | { kind: "start-note" }
  /** `personaId` is null when the user didn't name one ("start a
   *  conversation") — the caller keeps whatever persona is already active. */
  | { kind: "start-conversation"; personaId: string | null }
  | { kind: "stop" };

export interface VoiceCommandPersona {
  id: string;
  name: string;
}

function escapeRegExp(value: string): string {
  return value.replace(/[.*+?^${}()|[\]\\]/g, "\\$&");
}

// Same greeting list as prompts.ts's address detection, so "Hey Aral, take
// notes" works exactly like "Aral, take notes".
const GREETINGS = new Set([
  "hey", "hi", "hello", "ok", "okay", "hallo", "bonjour", "salut",
  "hola", "olá", "oi", "привет", "здравствуйте", "你好", "您好", "嗨",
  "こんにちは", "안녕", "안녕하세요",
]);

/** Whitespace and sentence punctuation, ASCII + CJK. */
const PUNCT = /^[\s,.!:;?—、，。！：；？]+|[\s,.!:;?—、，。！：；？]+$/g;

function tokenize(text: string): string[] {
  return text
    .split(/[\s]+/)
    .map((token) => token.replace(PUNCT, ""))
    .filter(Boolean);
}

function levenshtein(a: string, b: string): number {
  if (a === b) return 0;
  if (!a.length) return b.length;
  if (!b.length) return a.length;
  let prev = Array.from({ length: b.length + 1 }, (_, i) => i);
  for (let i = 1; i <= a.length; i++) {
    const row = [i];
    for (let j = 1; j <= b.length; j++) {
      row[j] = Math.min(
        prev[j] + 1,
        row[j - 1] + 1,
        prev[j - 1] + (a[i - 1] === b[j - 1] ? 0 : 1),
      );
    }
    prev = row;
  }
  return prev[b.length];
}

/**
 * Whether a spoken token is (a mishearing of) one word of the agent's name.
 *
 * Speech-to-text mangles short proper nouns constantly — real captures of
 * "Aral" have come back as "Aro", "Ahral", and "Arrow" — so an exact match
 * would make commands feel broken more often than not. The looseness is
 * safe here only because it's paired with the whole-utterance rule below:
 * a near-miss on the name does nothing unless everything *after* it is
 * exactly a command phrase and nothing else.
 */
function isNameToken(token: string, word: string): boolean {
  const t = token.toLowerCase();
  const w = word.toLowerCase();
  if (t === w) return true;
  // Scaled to length so short names don't swallow common words.
  const budget = w.length >= 4 ? 2 : 1;
  return levenshtein(t, w) <= budget;
}

/** How many leading tokens name the agent, or 0 if they don't. */
function matchNameTokens(tokens: string[], terms: string[]): number {
  for (const term of terms) {
    const words = term.trim().toLowerCase().split(/\s+/).filter(Boolean);
    if (!words.length || words.length > tokens.length) continue;
    if (words.every((word, i) => isNameToken(tokens[i], word))) return words.length;
  }
  return 0;
}

/** Anchors a pattern to the whole remainder. */
function whole(pattern: string): RegExp {
  return new RegExp(`^(?:${pattern})$`, "iu");
}

const ARTICLE = "(?:\\s+(?:a|an|the|my|new))*";

// "stop", "stop it", "end the conversation", "finish note", "stop recording"
const STOP = whole(
  `(?:stop|end|finish|cancel)(?:\\s+(?:it|that|this))?${ARTICLE}` +
    `(?:\\s+(?:conversation|conversations|call|note|notes|recording|meeting))?`,
);

// "start notes", "take a note", "new note", "note taking", "notes"
const NOTE = whole(
  `(?:(?:start|take|begin|new|create|make|open)${ARTICLE}\\s+)?` +
    `(?:note|notes)(?:\\s+(?:mode|taking))?`,
);

// "start a conversation", "begin call", "new chat", "conversation"
const CONVERSATION = whole(
  `(?:(?:start|begin|new|open|create)${ARTICLE}\\s+)?` +
    `(?:conversation|conversations|call|chat)(?:\\s+mode)?`,
);

/** "start support", "support", "use the sales persona", "switch to meeting" */
function personaPattern(name: string): RegExp {
  const escaped = escapeRegExp(name.trim()).replace(/\s+/g, "\\s+");
  return whole(
    `(?:(?:start|begin|open|use|switch\\s+to|go\\s+to)${ARTICLE}\\s+)?` +
      `${escaped}` +
      `(?:\\s+(?:persona|mode|conversation|call))?`,
  );
}

/** Guard against a runaway strip on pathological input. */
const MAX_NAME_REPEATS = 3;

/**
 * Returns the command the utterance asks for, or `null` when it isn't one
 * (ordinary dictation, or an agent question — both handled elsewhere).
 *
 * Checked in order: stop, then notes, then a named persona, then a generic
 * conversation. Order matters where vocabularies overlap — "stop meeting"
 * is a stop, not the Meeting persona; a persona literally named "Notes"
 * would lose to the note command.
 */
export function detectVoiceCommand(
  text: string,
  agentName: string | null,
  aliases: string[] | undefined,
  personas: VoiceCommandPersona[],
): VoiceCommand | null {
  const terms = [agentName ?? "", ...(aliases ?? [])].filter((t) => t.trim());
  if (terms.length === 0) return null;

  const tokens = tokenize(text);
  let i = 0;
  if (i < tokens.length && GREETINGS.has(tokens[i].toLowerCase())) i++;

  // People stutter the wake word, and the recognizer doubles it up on its
  // own ("Aro Aro start notes", "Aral Ahral start notes") — strip every
  // leading token that names the agent, not just the first.
  let repeats = 0;
  while (repeats < MAX_NAME_REPEATS) {
    const consumed = matchNameTokens(tokens.slice(i), terms);
    if (!consumed) break;
    i += consumed;
    repeats++;
  }
  if (repeats === 0) return null;

  const rest = tokens.slice(i).join(" ");
  // A bare "Aral" with nothing after it isn't a command.
  if (!rest) return null;

  if (STOP.test(rest)) return { kind: "stop" };
  if (NOTE.test(rest)) return { kind: "start-note" };

  for (const persona of personas) {
    if (persona.name.trim() && personaPattern(persona.name).test(rest)) {
      return { kind: "start-conversation", personaId: persona.id };
    }
  }

  if (CONVERSATION.test(rest)) return { kind: "start-conversation", personaId: null };

  return null;
}
