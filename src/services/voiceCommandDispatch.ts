import type { VoiceCommand, VoiceCommandDetection, VoiceCommandPersona } from "@/config/voiceCommands";
import { dispatchCaptureIntent } from "@/services/tauriApi";
import {
  resolveVoiceIntent,
  type CommandAsrRetry,
  type VoiceIntentDiagnostic,
} from "@/services/voiceIntentResolver";

export type CaptureIntentKind = "note" | "conversation" | "stop" | "settings";

export interface VoiceCommandDispatchPort {
  dispatchCaptureIntent: (kind: CaptureIntentKind, personaId?: string | null) => Promise<number>;
}

const tauriDispatchPort: VoiceCommandDispatchPort = {
  dispatchCaptureIntent,
};

export type VoiceCommandExecutionCode =
  | "candidate_evaluated"
  | "dispatch_started"
  | "intent_created"
  | "dispatch_failed";

export interface VoiceCommandExecutionDiagnostic {
  code: VoiceCommandExecutionCode;
  action?: VoiceCommand["kind"];
  intentId?: number;
  detection?: VoiceCommandDetection["diagnostics"];
  resolver?: VoiceIntentDiagnostic;
}

export interface CompletedVoiceCommandResult {
  handled: boolean;
  detection: VoiceCommandDetection;
  intentId: number | null;
}

/**
 * Routes a parsed command through the durable capture-intent boundary. The
 * command recognizer and this adapter stay independent from the Conversation
 * UI so the same controller can be exercised from a completed transcription
 * and extended without adding another cloud request.
 */
export async function dispatchVoiceCommand(
  command: VoiceCommand,
  port: VoiceCommandDispatchPort = tauriDispatchPort,
): Promise<number> {
  switch (command.kind) {
    case "start-note":
      return port.dispatchCaptureIntent("note");
    case "start-conversation":
      return port.dispatchCaptureIntent("conversation", command.personaId);
    case "stop":
      return port.dispatchCaptureIntent("stop");
    case "open-settings":
      return port.dispatchCaptureIntent("settings");
  }
}

/** Production completed-transcription boundary used by the recording hook's
 * pre-dictionary, pre-enhancement, pre-paste command interception. */
export async function dispatchCompletedVoiceCommand(
  text: string,
  agentName: string | null,
  aliases: string[] | undefined,
  personas: VoiceCommandPersona[],
  port: VoiceCommandDispatchPort = tauriDispatchPort,
  diagnose?: (event: VoiceCommandExecutionDiagnostic) => void,
  retry?: CommandAsrRetry,
  applicationLanguage?: string,
  allowBareWakeRecovery = false,
): Promise<CompletedVoiceCommandResult> {
  const resolution = await resolveVoiceIntent(
    text,
    agentName ?? "",
    aliases ?? [],
    personas,
    retry,
    diagnose ? (event) => diagnose({ code: "candidate_evaluated", resolver: event }) : undefined,
    applicationLanguage,
    allowBareWakeRecovery,
  );
  const detection = resolution.detection;
  diagnose?.({ code: "candidate_evaluated", detection: detection.diagnostics });
  if (!detection.match) return { handled: false, detection, intentId: null };

  const action = detection.match.command.kind;
  diagnose?.({ code: "dispatch_started", action });
  try {
    const intentId = await dispatchVoiceCommand(detection.match.command, port);
    diagnose?.({ code: "intent_created", action, intentId });
    return { handled: true, detection, intentId };
  } catch (error) {
    diagnose?.({ code: "dispatch_failed", action });
    throw error;
  }
}
