import {
  buildCommandRetryPrompt,
  detectVoiceCommandWithDiagnostics,
  isCommandRetryCandidate,
  matchBareLowRiskRetryAction,
  type VoiceCommandDetection,
  type VoiceCommandPersona,
} from "@/config/voiceCommands";

export type VoiceIntentStage = "fast_path" | "phonetic" | "retry" | "rejected";
export type VoiceIntentDiagnosticCode =
  | "raw_candidate_received"
  | "wake_match"
  | "fast_path_action_match"
  | "phonetic_recovery_match"
  | "retry_requested"
  | "retry_completed"
  | "retry_failed"
  | "final_action"
  | "rejected";

export interface VoiceIntentDiagnostic {
  code: VoiceIntentDiagnosticCode;
  wakeMatch?: VoiceCommandDetection["diagnostics"]["wakeMatch"];
  action?: VoiceCommandDetection["diagnostics"]["action"];
  reason?: VoiceCommandDetection["diagnostics"]["rejectionReason"] | "retry_unavailable" | "retry_failed";
  latencyMs?: number;
}

export interface VoiceIntentResolution {
  detection: VoiceCommandDetection;
  stage: VoiceIntentStage;
  retryAttempted: boolean;
}

export type CommandAsrRetry = (prompt: string) => Promise<string | null>;

function matchedStage(detection: VoiceCommandDetection): VoiceIntentStage {
  return detection.match?.matchType === "exact" ? "fast_path" : "phonetic";
}

/**
 * Resolve one completed ASR result. The retry callback is deliberately a
 * single function invocation owned by this resolver, which makes a retry
 * loop impossible even when provider code fails or returns another near miss.
 */
export async function resolveVoiceIntent(
  rawText: string,
  agentName: string,
  aliases: string[],
  personas: VoiceCommandPersona[],
  retry?: CommandAsrRetry,
  diagnose?: (event: VoiceIntentDiagnostic) => void,
  applicationLanguage?: string,
  allowBareWakeRecovery = false,
): Promise<VoiceIntentResolution> {
  diagnose?.({ code: "raw_candidate_received" });
  const initial = detectVoiceCommandWithDiagnostics(rawText, agentName, aliases, personas);
  diagnose?.({ code: "wake_match", wakeMatch: initial.diagnostics.wakeMatch });

  if (initial.match) {
    const stage = matchedStage(initial);
    diagnose?.({
      code: stage === "fast_path" ? "fast_path_action_match" : "phonetic_recovery_match",
      action: initial.diagnostics.action,
    });
    diagnose?.({ code: "final_action", action: initial.diagnostics.action });
    return { detection: initial, stage, retryAttempted: false };
  }

  const bareRetryAction = allowBareWakeRecovery && initial.diagnostics.wakeMatch === "none"
    ? matchBareLowRiskRetryAction(rawText, personas)
    : null;
  if (!bareRetryAction && !isCommandRetryCandidate(initial, rawText, personas)) {
    diagnose?.({ code: "rejected", reason: initial.diagnostics.rejectionReason });
    return { detection: initial, stage: "rejected", retryAttempted: false };
  }
  if (!retry) {
    diagnose?.({ code: "rejected", reason: "retry_unavailable" });
    return { detection: initial, stage: "rejected", retryAttempted: false };
  }

  const started = performance.now();
  diagnose?.({ code: "retry_requested" });
  try {
    const retryText = await retry(buildCommandRetryPrompt(agentName, aliases, applicationLanguage, personas));
    diagnose?.({ code: "retry_completed", latencyMs: Math.round(performance.now() - started) });
    if (retryText?.trim()) {
      const recovered = detectVoiceCommandWithDiagnostics(retryText, agentName, aliases, personas);
      const sameBareAction = !bareRetryAction
        || (recovered.match?.command.kind === bareRetryAction.kind
          && (recovered.match.command.kind !== "start-conversation"
            || bareRetryAction.kind !== "start-conversation"
            || recovered.match.command.personaId === bareRetryAction.personaId));
      if (recovered.match && recovered.match.command.kind !== "stop" && sameBareAction) {
        diagnose?.({ code: "final_action", action: recovered.diagnostics.action });
        return { detection: recovered, stage: "retry", retryAttempted: true };
      }
    }
  } catch {
    diagnose?.({ code: "retry_failed", latencyMs: Math.round(performance.now() - started), reason: "retry_failed" });
  }

  diagnose?.({ code: "rejected", reason: initial.diagnostics.rejectionReason });
  return { detection: initial, stage: "rejected", retryAttempted: true };
}
