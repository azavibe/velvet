import { invoke } from "@tauri-apps/api/core";
import { emit, listen, type UnlistenFn } from "@tauri-apps/api/event";
import {
  normalizeDictionary,
  type DictionaryEntry,
} from "@/models/dictionary";

// Audio
export interface AudioDevice {
  id: string;
  name: string;
  is_default: boolean;
}

export async function listAudioDevices(): Promise<AudioDevice[]> {
  return invoke("list_audio_devices");
}

export async function startRecording(deviceId?: string): Promise<void> {
  return invoke("start_recording", { deviceId });
}

export async function stopRecording(): Promise<number[]> {
  return invoke("stop_recording");
}

export async function getAudioLevel(): Promise<number> {
  return invoke("get_audio_level");
}

export async function onAudioLevel(
  callback: (level: number) => void,
): Promise<UnlistenFn> {
  return listen<{ level: number }>("audio-level", (event) => {
    callback(event.payload.level);
  });
}

export async function onRecordingError(
  callback: (error: string) => void,
): Promise<UnlistenFn> {
  return listen<{ error: string }>("recording-error", (event) => {
    callback(event.payload.error);
  });
}

// Transcription
/**
 * Result of a transcription call. `text` is the cleaned-up output.
 * `detected_language` is what the model reported during language ID
 * (present only when the user requested auto-detect AND the provider
 * supports detection, such as OpenAI/Groq's verbose_json response).
 * Forward it into `processReasoning` so AI enhancement runs with the
 * resolved language instead of "auto".
 */
export interface TranscriptionResult {
  text: string;
  detected_language: string | null;
}

export async function transcribeCloud(
  audioData: number[],
  provider: string,
  apiKey: string,
  model: string,
  language?: string,
  secondaryLanguage?: string,
  dictionary?: string[],
  /** Agent names and explicit correction targets that must never be stripped
   *  as prompt echoes because they may be exactly what the user spoke. */
  protectedTerms?: string[],
): Promise<TranscriptionResult> {
  return invoke("transcribe_cloud", {
    audioData,
    provider,
    apiKey,
    model,
    language,
    secondaryLanguage,
    dictionary: dictionary ?? [],
    protectedTerms: protectedTerms ?? [],
  });
}

// Reasoning
export async function processReasoning(
  text: string,
  model: string,
  provider: string,
  systemPrompt: string,
  apiKey: string,
  maxTokens?: number,
  temperature?: number,
  language?: string,
): Promise<string> {
  return invoke("process_reasoning", {
    text,
    model,
    provider,
    systemPrompt,
    apiKey,
    maxTokens,
    temperature,
    language,
  });
}

// Changelog
export async function readChangelog(): Promise<string> {
  return invoke("read_changelog");
}

// Database
export interface Transcription {
  id: number;
  timestamp: string;
  original_text: string;
  processed_text: string | null;
  is_processed: boolean;
  processing_method: string;
  agent_name: string | null;
  error: string | null;
  duration_ms: number | null;
  word_count: number | null;
  audio_path: string | null;
}

export async function saveTranscription(
  originalText: string,
  processedText: string | null,
  processingMethod: string,
  agentName: string | null,
  error: string | null,
  durationMs: number | null,
  audioData?: number[] | null,
): Promise<number> {
  return invoke("save_transcription", {
    originalText,
    processedText,
    processingMethod,
    agentName,
    error,
    durationMs,
    audioData: audioData ?? null,
  });
}

// Stats
export type StatsPeriod = "today" | "week" | "all";

export interface StatsPayload {
  total_seconds: number;
  total_words: number;
  total_recordings: number;
  avg_seconds: number;
  avg_words: number;
}

export async function getStats(period: StatsPeriod): Promise<StatsPayload> {
  return invoke("get_stats", { period });
}

export async function getTranscriptions(
  limit: number,
  offset: number,
): Promise<Transcription[]> {
  return invoke("get_transcriptions", { limit, offset });
}

export async function deleteTranscription(id: number): Promise<void> {
  return invoke("delete_transcription", { id });
}

export async function clearTranscriptions(): Promise<void> {
  return invoke("clear_transcriptions");
}

// Clipboard
export async function pasteText(text: string): Promise<void> {
  return invoke("paste_text", { text });
}

export async function readClipboard(): Promise<string> {
  return invoke("read_clipboard");
}

/** Set the clipboard without pasting (Live polish fallback when an in-place
 *  swap isn't safe). */
export async function setClipboardText(text: string): Promise<void> {
  return invoke("set_clipboard_text", { text });
}

// Settings
export async function getSetting<T = unknown>(key: string): Promise<T | null> {
  return invoke("get_setting", { key });
}

export async function setSetting(key: string, value: unknown): Promise<void> {
  return invoke("set_setting", { key, value });
}

export async function getAllSettings(): Promise<Record<string, unknown>> {
  return invoke("get_all_settings");
}

// App
export async function quitApp(): Promise<void> {
  return invoke("quit_app");
}

export async function showSettings(): Promise<void> {
  return invoke("show_settings");
}

export async function showConversationWindow(): Promise<void> {
  return invoke("show_conversation_window");
}

export async function hideConversationWindow(): Promise<void> {
  return invoke("hide_conversation_window");
}

// --- Settings convenience helpers ---

// Agent name
const DEFAULT_AGENT_NAME = "Aral";

export async function getAgentName(): Promise<string> {
  const name = await getSetting<string>("agentName");
  return name || DEFAULT_AGENT_NAME;
}

export async function setAgentName(name: string): Promise<void> {
  return setSetting("agentName", name);
}

// API keys (stored in tauri-plugin-store settings.json)
const API_KEY_MAP: Record<string, string> = {
  openai: "openaiApiKey",
  anthropic: "anthropicApiKey",
  gemini: "geminiApiKey",
  groq: "groqApiKey",
  mistral: "mistralApiKey",
  qwen: "qwenApiKey",
  openrouter: "openrouterApiKey",
};

export async function getApiKey(provider: string): Promise<string> {
  const key = API_KEY_MAP[provider] ?? `${provider}ApiKey`;
  const value = await getSetting<string>(key);
  return value || "";
}

export async function setApiKey(provider: string, apiKey: string): Promise<void> {
  const key = API_KEY_MAP[provider] ?? `${provider}ApiKey`;
  return setSetting(key, apiKey);
}

// Custom dictionary
export async function getCustomDictionary(): Promise<DictionaryEntry[]> {
  const dict = await getSetting<unknown>("customDictionary");
  return normalizeDictionary(dict);
}

export async function setCustomDictionary(
  entries: DictionaryEntry[],
): Promise<void> {
  return setSetting("customDictionary", normalizeDictionary(entries));
}

// Agent aliases
export async function getAgentAliases(): Promise<string[]> {
  const aliases = await getSetting<string[]>("agentAliases");
  return aliases || [];
}

export async function setAgentAliases(aliases: string[]): Promise<void> {
  return setSetting("agentAliases", aliases);
}

// ---- Live mode ----

export interface LiveUtterancePayload {
  session_id: number;
  text: string;
  utterance_seq: number;
}

export interface LiveErrorPayload {
  session_id: number;
  message: string;
  kind: "AuthFailed" | "RateLimited" | "NetworkDrop" | "ServerError" | "MaxMessageExceeded" | "BadResponse";
}

export type SwapResult = "Swapped" | "SkippedFocusDrift" | "SkippedNoChange";

export async function startLiveSession(args: {
  providerId: string;
  model: string;
  language: string | null;
  dictionary: string[];
  apiKey: string;
  expectedHwnd: number | null;
}): Promise<number> {
  return invoke<number>("start_live_session", {
    providerId: args.providerId,
    model: args.model,
    language: args.language,
    dictionary: args.dictionary,
    apiKey: args.apiKey,
    expectedHwnd: args.expectedHwnd,
  });
}

export async function stopLiveSession(sessionId: number): Promise<void> {
  await invoke("stop_live_session", { sessionId });
}

export async function cancelLiveSession(sessionId: number): Promise<void> {
  await invoke("cancel_live_session", { sessionId });
}

/** Result of typing one Live chunk: UTF-16 units sent, and the focus target
 *  (top-level window + focused control) they landed in. `scopable` is false for
 *  web/Electron render surfaces where many boxes share one control HWND. */
export interface TypedChunk {
  chars: number;
  window: number;
  control: number;
  scopable: boolean;
}

export async function typeTextChunk(text: string): Promise<TypedChunk> {
  return invoke<TypedChunk>("type_text_chunk", { text });
}

export async function swapTypedText(
  backspaceCount: number,
  newText: string,
  expectedHwnd: number | null,
  expectedControl: number | null,
): Promise<SwapResult> {
  return invoke<SwapResult>("swap_typed_text_cmd", {
    backspaceCount,
    newText,
    expectedHwnd,
    expectedControl,
  });
}

export async function getForegroundWindow(): Promise<number> {
  return invoke<number>("get_foreground_window");
}

export async function getForegroundWindowClass(): Promise<string | null> {
  return invoke<string | null>("get_foreground_window_class");
}

/** Where the next keystrokes would land: the foreground window and the focused
 *  control within it. `scopable` is false for web/Electron render surfaces
 *  (many text boxes behind one control HWND). */
export interface FocusTarget {
  window: number;
  control: number;
  scopable: boolean;
}

export async function getFocusTarget(): Promise<FocusTarget> {
  return invoke<FocusTarget>("get_focus_target");
}

export async function onLiveUtterance(
  callback: (payload: LiveUtterancePayload) => void,
): Promise<UnlistenFn> {
  return listen<LiveUtterancePayload>("live-utterance", (e) => callback(e.payload));
}

export async function onLiveError(
  callback: (payload: LiveErrorPayload) => void,
): Promise<UnlistenFn> {
  return listen<LiveErrorPayload>("live-error", (e) => callback(e.payload));
}

export async function onLiveSessionClosed(
  callback: (sessionId: number) => void,
): Promise<UnlistenFn> {
  return listen<number>("live-session-closed", (e) => callback(e.payload));
}

// Settings change event
export async function onSettingsChanged(callback: () => void): Promise<UnlistenFn> {
  return listen("settings-changed", () => callback());
}

// --- Conversations (live call copilot) ---

export type ConversationChannel = "me" | "them";

export interface ConversationUtteranceEvent {
  id: number;
  conversation_id: number;
  channel: ConversationChannel;
  started_at_ms: number;
  text: string;
}

export interface ConversationSuggestionEvent {
  id: number;
  conversation_id: number;
  created_at_ms: number;
  persona_name: string | null;
  text: string;
}

export interface ConversationErrorEvent {
  conversation_id: number;
  message: string;
}

export interface ConversationSummary {
  id: number;
  started_at: string;
  ended_at: string | null;
  title: string | null;
  persona_name: string | null;
  audio_path_me: string | null;
  audio_path_them: string | null;
  snippet: string | null;
}

export interface ConversationDetail {
  conversation: ConversationSummary;
  utterances: ConversationUtteranceEvent[];
  suggestions: ConversationSuggestionEvent[];
}

export async function startConversation(
  micDeviceId: string | undefined,
  groqApiKey: string,
  personaName: string | undefined,
): Promise<number> {
  return invoke("start_conversation", { micDeviceId, groqApiKey, personaName });
}

export async function stopConversation(conversationId: number, title?: string): Promise<void> {
  return invoke("stop_conversation", { conversationId, title });
}

export async function getConversationAudioLevels(): Promise<[number, number]> {
  return invoke("get_conversation_audio_levels");
}

export async function isConversationActive(): Promise<boolean> {
  return invoke("is_conversation_active");
}

export async function getConversationError(): Promise<string | null> {
  return invoke("get_conversation_error");
}

export async function listConversations(limit: number, offset: number): Promise<ConversationSummary[]> {
  return invoke("list_conversations", { limit, offset });
}

export async function getConversation(conversationId: number): Promise<ConversationDetail> {
  return invoke("get_conversation", { conversationId });
}

export async function deleteConversation(conversationId: number): Promise<void> {
  return invoke("delete_conversation", { conversationId });
}

export async function generateSuggestion(
  conversationId: number,
  personaSystemPrompt: string,
  personaName: string | undefined,
  model: string,
  provider: string,
  apiKey: string,
): Promise<string> {
  return invoke("generate_suggestion", {
    conversationId,
    personaSystemPrompt,
    personaName,
    model,
    provider,
    apiKey,
  });
}

export async function onConversationUtterance(
  callback: (payload: ConversationUtteranceEvent) => void,
): Promise<UnlistenFn> {
  return listen<ConversationUtteranceEvent>("conversation-utterance", (e) => callback(e.payload));
}

export async function onConversationSuggestion(
  callback: (payload: ConversationSuggestionEvent) => void,
): Promise<UnlistenFn> {
  return listen<ConversationSuggestionEvent>("conversation-suggestion", (e) => callback(e.payload));
}

export async function onConversationError(
  callback: (payload: ConversationErrorEvent) => void,
): Promise<UnlistenFn> {
  return listen<ConversationErrorEvent>("conversation-error", (e) => callback(e.payload));
}

export interface ConversationStartedEvent {
  conversation_id: number;
  persona_name: string | null;
}

/** Broadcast to every window regardless of which one issued the start/stop
 *  command, so any window's UI reflects the true global state — there's
 *  exactly one conversation possible at a time (the backend audio capture
 *  is a single global resource). */
export async function onConversationStarted(
  callback: (payload: ConversationStartedEvent) => void,
): Promise<UnlistenFn> {
  return listen<ConversationStartedEvent>("conversation-started", (e) => callback(e.payload));
}

export async function onConversationStopped(
  callback: (conversationId: number) => void,
): Promise<UnlistenFn> {
  return listen<number>("conversation-stopped", (e) => callback(e.payload));
}

// --- Notes (mic-only capture with pause/resume) ---

export interface Note {
  id: number;
  created_at: string;
  updated_at: string;
  title: string | null;
  raw_transcript: string;
  body_markdown: string | null;
  audio_path: string | null;
  tags: string[];
}

export interface NoteUtteranceEvent {
  note_id: number;
  started_at_ms: number;
  text: string;
}

export interface NoteErrorEvent {
  note_id: number;
  message: string;
}

/** `noteId: undefined` creates a new note; pass an existing id to append
 *  ("Append Dictation" from a History card). Either way resolves to the
 *  note id capture is targeting. */
export async function startNoteCapture(
  micDeviceId: string | undefined,
  groqApiKey: string,
  noteId?: number,
): Promise<number> {
  return invoke("start_note_capture", { micDeviceId, groqApiKey, noteId });
}

export async function pauseNoteCapture(): Promise<void> {
  return invoke("pause_note_capture");
}

export async function resumeNoteCapture(): Promise<void> {
  return invoke("resume_note_capture");
}

export async function stopNoteCapture(): Promise<void> {
  return invoke("stop_note_capture");
}

export async function isNoteCaptureActive(): Promise<boolean> {
  return invoke("is_note_capture_active");
}

export async function isNoteCapturePaused(): Promise<boolean> {
  return invoke("is_note_capture_paused");
}

export async function getNoteCaptureError(): Promise<string | null> {
  return invoke("get_note_capture_error");
}

export async function listNotes(limit: number, offset: number): Promise<Note[]> {
  return invoke("list_notes", { limit, offset });
}

export async function getNote(noteId: number): Promise<Note> {
  return invoke("get_note", { noteId });
}

export async function updateNote(noteId: number, title: string | null, rawTranscript: string): Promise<void> {
  return invoke("update_note", { noteId, title, rawTranscript });
}

export async function setNoteTitle(noteId: number, title: string): Promise<void> {
  return invoke("set_note_title", { noteId, title });
}

export async function deleteNote(noteId: number): Promise<void> {
  return invoke("delete_note", { noteId });
}

export async function cleanupNote(
  noteId: number,
  model: string,
  provider: string,
  apiKey: string,
): Promise<string> {
  return invoke("cleanup_note", { noteId, model, provider, apiKey });
}

export async function onNoteStarted(callback: (noteId: number) => void): Promise<UnlistenFn> {
  return listen<number>("note-started", (e) => callback(e.payload));
}

export async function onNoteStopped(callback: () => void): Promise<UnlistenFn> {
  return listen<void>("note-stopped", () => callback());
}

export async function onNoteUtterance(callback: (payload: NoteUtteranceEvent) => void): Promise<UnlistenFn> {
  return listen<NoteUtteranceEvent>("note-utterance", (e) => callback(e.payload));
}

export async function onNoteError(callback: (payload: NoteErrorEvent) => void): Promise<UnlistenFn> {
  return listen<NoteErrorEvent>("note-error", (e) => callback(e.payload));
}

/** "Append Dictation" on a History card: the Settings window can't start
 *  capture itself (that lives in the Conversation window), so it shows that
 *  window and asks it to resume capture into this note via a plain
 *  frontend-to-frontend event — no backend command needed since both sides
 *  are already-permitted windows listening/emitting on the same channel. */
export async function requestNoteAppend(noteId: number): Promise<void> {
  await emit("note-append-requested", noteId);
}

export async function onNoteAppendRequested(callback: (noteId: number) => void): Promise<UnlistenFn> {
  return listen<number>("note-append-requested", (e) => callback(e.payload));
}
