# TODO

## Conversational assistant roadmap

Detailed [implementation plan](plans/conversational-assistant.md).

### v0.8.10 — Recording visibility and release correction

- [ ] Show recording state in the system tray when the floating bubble is hidden.
  - [x] Switch the shared tray icon to magenta for Standard, Live, Conversation, and Notes capture without changing the bubble animation.
  - [x] Restore the idle icon on stop, failure, or note pause, with stale-session protection for rapid recording restarts.
  - [ ] Verify all capture modes, note pause/resume, hidden-bubble recording, and rapid stop/start behavior in the packaged Windows tray.
- [ ] Remove repeated configured-agent wake-word echoes without breaking fuzzy voice commands or legitimate interior mentions.
  - [x] Sanitize repeated configured-name and alias echoes at utterance boundaries before command routing, enhancement, paste, and Live typing while preserving raw History text.
  - [ ] Verify repeated leading and trailing wake-name captures in Standard and Live modes on Windows with runtime name changes, fuzzy variants, commands, direct questions, and legitimate interior mentions.

### v0.8.9 — Repairs and stabilization

- [x] Repair archived-audio persistence so dictation, conversation, and note recordings have explicit saving, ready, missing, or failed states.
  - [x] Serialize archive writes and finalization so History cannot observe an incomplete WAV.
  - [x] Track every note append segment and delete all recordings associated with its source.
- [ ] Add secure History audio playback for dictations, conversation Me/Them tracks, and note segments.
  - [x] Support play, pause, resume, seek, restart, stop, duration, and cancellation when switching items.
  - [x] Restrict WebView/open-file access to validated files under the application recordings directory.
  - [ ] Complete Windows WebView2 runtime playback and Task Manager memory verification.
    - [ ] Verify one long note segment and Play All complete without a protocol-thread panic, skip an unavailable segment, and continue in stored segment order.
    - [ ] Verify dictation, Conversation Me/Them, and note playback starts at byte zero and seeks repeatedly through the middle and near EOF in packaged WebView2.
    - [ ] Compare SQLite source/asset counts and rendered source IDs before and after repeated Play, Pause, Seek, Restart, Stop, and Play All; confirm sequential range requests remain read-only.
    - [ ] Measure idle, History-visible, active-playback, and post-stop Windows memory and confirm playback returns close to baseline.
- [ ] Diagnose startup lag and ensure archive recovery does not block the interface unnecessarily.
  - [x] Add development-only timing marks for initialization, migrations, archive recovery, WebView/React startup, shortcut registration, and the first History/Notes query.
  - [x] Manage the database before the one-time WAV recovery pass and refresh History after explicit recovery completion or failure.
  - [ ] Measure cold/second development startup and packaged-release startup on Windows before closing the diagnosis.
- [ ] Fix the global hotkey becoming unresponsive after extended use and window/recording transitions.
  - [x] Serialize registration, replacement, retry, and cleanup through one lifecycle owner with stable callbacks and duplicate-listener protection.
  - [x] Surface registration failure through the overlay and cover stale cleanup/retry ordering with regression tests.
  - [ ] Run the Windows 20-cycle start/stop and panel open/close reliability matrix.
- [ ] Improve whole-utterance voice-command matching for conservative ASR sound-alikes.
  - [x] Match configured wake names and aliases with exact preference, short command-scoped tolerance, and privacy-safe exact/fuzzy diagnostics.
  - [x] Cover reported note variants, Support/Conversation, multilingual greetings, aliases, and false-positive ordinary sentences in regression tests.
  - [x] Resolve raw ASR through one declarative exact/phonetic command registry before dictionary replacement, enhancement, paste, or History persistence.
  - [x] Reuse the original recording for at most one bounded command-focused retry with the selected prompt-capable provider/model, deterministic settings, timeout fallback, and no duplicate persistence.
  - [x] Route Settings/preferences through the durable target-window intent and acknowledgement path without treating addressed questions as application commands.
  - [x] Keep the configured agent name and aliases out of the normal Whisper context prompt so a spoken leading wake name is not suppressed as repeated prior text.
  - [x] Recover a provider-elided wake only for sub-six-second exact low-risk command phrases when one retry restores the configured name and the same action; never apply this to Stop.
  - [x] Drop the confirmed whole-output “How can I assist you today?” Whisper greeting hallucination, including repeated loops.
  - [ ] Verify the configured-name command matrix on Windows with the reported microphone captures.
    - [ ] Set the Agent Name preference to Tom and verify “Agent Tom, start note” plus a capture resembling “Thumb, Stark Nose. Thank you” opens Notes exactly once and is not pasted or saved.
    - [ ] Set the Agent Name preference to Agenda and, in separate recordings, verify the raw result retains the leading name and executes notes, start notes, call, start call, start conversation, support, settings, and open settings, including captures resembling “Agenda Noty” and “Agenda Kolejny.”
    - [ ] Verify a short capture whose first result loses “Agenda” can recover the same low-risk action once, while bare Stop and a retry that changes actions remain ordinary speech.
    - [ ] Say only “Agenda” several times and confirm stock “Hello/How can I assist you today?” output is discarded rather than pasted or saved.
    - [ ] Repeat the command-focused retry with a supported provider and confirm it keeps the selected model; repeat with an unsupported provider and confirm ordinary dictation continues without a second cloud request.
    - [ ] Confirm “Agenda, what’s your name?”, “what settings do you have?”, and “what agents are available?” remain non-command conversational candidates.
    - [ ] Repeat the matrix with the target window initially absent, already open, and slow to mount; confirm each intent is acknowledged exactly once.
    - [ ] Change the configured agent name and aliases at runtime, then run two commands in close succession and repeat a completed command.
    - [ ] Confirm each consumed command opens/focuses the intended mode/persona, starts or requests the intended capture, and is not pasted or saved as ordinary dictation.
- [ ] Reduce the floating agent icon by 30%, remove its white stroke, and add lightweight active-state animation.
  - [x] Reduce the visible circle to 32px inside a 44px target, remove the border, and add recording/processing/error phases with reduced-motion CSS.
  - [ ] Complete Windows visual, CPU, and RAM verification for idle, recording, processing, and error states.
    - [ ] Verify light and dark backgrounds show exactly one 32px visible circle and no border, stroke, or transparent 44px ring.
    - [ ] Verify the clipped recording smoke is centered on the microphone circle and remains centered while dragging the overlay.
    - [ ] Verify processing uses the compact horizontal pulse without spinning/orbiting layers and stops all motion when idle or reduced motion is enabled.
- [ ] Rebrand the application from Whisperi to Agenda without losing existing user data or installer continuity.
  - [ ] Audit visible branding, package metadata, installer names, window titles, tray text, locales, documentation, and logs.
  - [ ] Preserve or migrate legacy database, recordings, settings, API keys, updater identity, and application-data paths.
  - [ ] Preserve the original MIT license and upstream attribution.
  - [ ] Verify upgrade installation from v0.8.8 and first-run migration on Windows.

### v0.9.0 — Direct conversation and text-to-speech

- [ ] Route non-command speech addressed to the configured agent into persistent direct conversation turns.
  - [ ] Reuse the existing conversation tables, reasoning settings, Conversation window, personas, and history.
  - [ ] Persist raw user text, reconciled user text, and assistant answers without creating duplicate transcription history.
  - [ ] Keep ordinary dictation pasting and whole-utterance voice commands unchanged.
- [ ] Add OpenRouter TTS using the existing API key and the default Qwen Audio 3.0 TTS Flash model.
  - [ ] Add a small Conversations-area TTS toggle/model setting with cached speech-model discovery and custom model support.
  - [ ] Reuse the History playback layer for MP3 playback and preserve visible text when synthesis fails.
- [ ] Add interruption and stale-result protection for direct answers and TTS.
  - [ ] Stop speech immediately when new user recording begins.
  - [ ] Prevent synthesized speaker output from becoming a new voice command or user turn.
- [ ] Extend Conversation to speakerphone and in-room calls without adding another capture mode.
  - [ ] Treat silent-loopback sessions as mixed microphone audio instead of labeling every speaker “Me.”
  - [ ] Accept configured-agent commands such as “start call” and “call” as Conversation aliases.
  - [ ] Automatically structure support-call cleanup around instructions, forms, payments, contacts, deadlines, and unresolved questions.
  - [ ] Reuse the existing conversation, audio, history, cleanup, persona, and suggestion infrastructure.

### v0.9.x — Automatic contextual speech correction

- [ ] Add conservative post-ASR reconciliation before command detection, enhancement, persistence, and direct reasoning.
  - [ ] Use language, current context, recent utterances, dictionary terms, known entities, and repeated high-confidence evidence.
  - [ ] Preserve raw ASR beside reconciled text and fall back safely on uncertainty or provider failure.
  - [ ] Promote only repeated consistent corrections and reduce confidence after contradiction or inactivity.
- [ ] Make contextual correction reversible, bounded, debuggable, and separate from normal prose enhancement.

### v0.10.0 — Long-term memory and knowledge

- [ ] Add a minimal SQLite-backed memory layer for entities, aliases, facts, relationships, sources, confidence, and contradiction state.
  - [ ] Extract memory automatically after completed turns, notes, and conversations without making ambiguous content permanent.
  - [ ] Retrieve only bounded recent context, rolling summaries, relevant SQLite results, aliases, recency, confidence, persona, and topic.
  - [ ] Remove or downgrade source-dependent memory when its dictation, note, or conversation is deleted.
- [ ] Add an automatic-memory toggle and reset action without introducing a graph editor or training interface.

### Future — Disclosed meeting participation

- [ ] Design a separate, explicitly disclosed meeting-participant mode compatible with existing microphone/loopback capture, two-channel transcripts, personas, suggestions, TTS, and barge-in.
  - [ ] Define participant consent, visible listening/speaking state, AI-voice disclosure, meeting context, summaries, decisions, and tasks.
  - [ ] Evaluate virtual microphones for Zoom/Teams only after the disclosed interaction model is approved.

## Live mode stabilization

- [ ] Remove "(Beta)" label after 2 consecutive minor releases with zero Live-mode-related issues + multi-provider validation.
- [ ] Auto-reconnect on transient network drops.
- [ ] OS keyring migration for API keys (`tauri-plugin-stronghold` or `keyring-rs`).
- [ ] Secure-window auto-pause (UAC, lsass, credential dialogs).
- [ ] Additional streaming providers: Deepgram Nova-3, AssemblyAI Universal-Streaming.
- [ ] In-app cost meter / session cost estimation.
- [ ] Voice-command corrections ("scratch that", "delete last sentence").
- [ ] Extend `sanitize_for_send_input` to strip OSC (`\x1B]`), DCS (`\x1B P`), and SS3 (`\x1B O`) escape sequences in addition to CSI (`\x1B[`). Low practical risk today since ASR backends don't emit them, but spec implies "all ANSI escapes."
- [ ] Consolidate `is_foreground_terminal` (ANSI/`GetClassNameA`) and `is_foreground_window_terminal_class` (Wide/`GetClassNameW`) in `clipboard/mod.rs` into a single Wide-string implementation.
- [ ] Wire `useLiveDictation` toast strings (and `useAudioRecording`'s) through `react-i18next` `t()` rather than hardcoded English — the i18n keys already exist in all 9 locales.
- [ ] Russian i18n: fix "Другой вкладка" → "Другой вкладке" (`ru.json` `transcription.live.apiKeyRequired` and `dictation.live.error.noApiKey`).
- [ ] Clean up dead `"processing"` variant in `useLiveDictation.ts` `LivePhase` union (only `"polishing"` is ever set in Live mode).
- [ ] Verify `Win32_System_Threading` Cargo feature is needed (added in Task 1 anticipating `AttachThreadInput`, but never used yet).
- [ ] Add `Drop` impl on `LiveSessionState` to abort active task handles on app shutdown (prevents detached tokio tasks if app exits mid-session).
- [ ] `useLiveDictation.ts` `Promise.race([enhance, timeout])` leaves the `setTimeout` running after enhance resolves — clear it on success to avoid stray unhandled-rejection warnings (and a duplicate timer under React StrictMode dev double-invoke).
- [ ] `notifyError` in `DictationOverlay.tsx` is wrapped in `useCallback([t])`; every i18n language change rebinds it and tears down/re-subscribes all 5 Live event listeners. Move `t` inside via a ref so the callback identity is stable.
- [ ] Replace `std::sync::Mutex` access on `samples_buf` in the audio-pump tokio task with `tokio::sync::Mutex` (or move the drain into `spawn_blocking`) — reduces executor jitter under cpal callback contention.
- [ ] Skip `commit_utterance()` in the soft-flush path when the loop exited via error AND when the provider is `ServerVad` — currently it's always sent, generating a spurious "buffer too small" server error event that the drain loop silently discards.
- [ ] Call `resampler.flush()` after the audio-pump main loop exits — currently the trailing interpolated sample is dropped (sub-ms audio loss; matters only on perfectly-aligned utterance-end boundaries).
- [ ] In `useLiveDictation.ts` `subscribe()`, register each unlisten function into `unlistenRef.current` immediately after each `await` resolves instead of all-at-once at the end — if a later `await` rejects, earlier successfully-registered listeners are currently leaked.
- [ ] **Per-box polish in web/Electron fields**: the post-stop polish swap falls back to clipboard for any browser/Electron field because many text boxes share one render HWND and can't be told apart by HWND (`is_web_render_class` denylist in `clipboard/mod.rs`). Use UI Automation (focused element + `TextPattern`/`ValuePattern`) to identify and scope to individual web boxes so single-field web dictation can auto-replace in place again instead of copying to the clipboard.
- [ ] **Caret-move-safe polish**: the scoped swap still backspaces from the current caret, so moving the caret within the same box (clicking elsewhere, manual edits) before stop deletes the wrong characters. Track the selection/caret offset, or offer a non-destructive replace, so an in-field caret move can't corrupt the swap.

## Bilingual language mode follow-ups

- [ ] **Live incremental refinement**: streaming rewrite of recent utterances when Live mode accumulates enough context to refine an earlier detected-language decision (spec §9).
- [ ] **Learned user edits**: automatically capture post-dictation corrections as suggested dictionary aliases. Manual canonical/alias rules and contextual/always policies now exist; this follow-up is the learning and confirmation workflow (spec §9).

## Dictation stability follow-ups (deferred from 2026-07-23 fixes)

- [ ] Apply the hallucination blocklist (`transcription/hallucination.rs`) to Live-mode utterances too — server VAD already gates silence, so deferred; wire into the utterance path if hallucinated utterances are ever reported in Live.
- [ ] Parse `no_speech_prob`/`avg_logprob` from `verbose_json` responses (whisper-1/Groq/Mistral only) as an additional no-speech signal; the recorder gate + blocklist cover the default gpt-4o path, which returns no such metadata.
- [ ] Consider dropping the conditioning sentence in Single/Bilingual prompts (auto mode already sends none) — it is the main text Whisper echoes on silence; the dictionary-only prompt is lower-risk.
