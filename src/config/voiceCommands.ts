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
 * triggering an action.
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
const GREETINGS = [
  "hey", "hi", "hello", "ok", "okay", "hallo", "bonjour", "salut",
  "hola", "olá", "oi", "привет", "здравствуйте", "你好", "您好", "嗨",
  "こんにちは", "안녕", "안녕하세요",
].map(escapeRegExp).join("|");

/** Whitespace and sentence punctuation, ASCII + CJK. */
const SEP = "[\\s,.!:;?—、，。！：；？]";

/**
 * Strips an optional greeting plus the agent name off the front.
 * Returns the remaining text, or `null` when the utterance isn't addressed
 * to the agent at all (in which case it's ordinary dictation, not a command).
 */
function stripAgentPrefix(text: string, terms: string[]): string | null {
  for (const term of terms) {
    const trimmed = term.trim();
    if (!trimmed) continue;
    const escaped = escapeRegExp(trimmed).replace(/\s+/g, "\\s+");
    const re = new RegExp(
      `^${SEP}*(?:(?:${GREETINGS})${SEP}+)?${escaped}${SEP}*`,
      "iu",
    );
    const match = text.match(re);
    // Require the match to actually contain the name, not just leading
    // whitespace matched by the optional groups.
    if (match && new RegExp(escaped, "iu").test(match[0])) {
      return text.slice(match[0].length);
    }
  }
  return null;
}

/** Anchors a pattern to the whole remainder, tolerating trailing punctuation. */
function whole(pattern: string): RegExp {
  return new RegExp(`^${SEP}*(?:${pattern})${SEP}*$`, "iu");
}

const ARTICLE = "(?:\\s+(?:a|an|the|my|new))*";

// "stop", "stop it", "end the conversation", "finish note", "stop recording"
const STOP = whole(
  `(?:stop|end|finish|cancel)(?:\\s+(?:it|that|this))?${ARTICLE}` +
    `(?:\\s+(?:conversation|conversations|call|note|notes|note-taking|recording|meeting))?`,
);

// "start notes", "take a note", "new note", "note taking", "notes"
const NOTE = whole(
  `(?:(?:start|take|begin|new|create|make|open)${ARTICLE}\\s+)?` +
    `(?:note|notes|note-taking|note taking)(?:\\s+(?:mode|taking))?`,
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

  const rest = stripAgentPrefix(text, terms);
  if (rest === null) return null;
  // A bare "Aral" with nothing after it isn't a command.
  if (!rest.trim()) return null;

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
