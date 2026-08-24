import {
  acknowledgeCaptureIntent,
  getPendingCaptureIntent,
  type CaptureIntent,
} from "@/services/tauriApi";

export interface CaptureIntentConsumerPort {
  getPendingCaptureIntent: (target?: "conversation" | "settings") => Promise<CaptureIntent | null>;
  acknowledgeCaptureIntent: (intentId: number) => Promise<boolean>;
}

export type CaptureIntentDiagnosticCode =
  | "target_not_ready"
  | "target_ready"
  | "intent_acknowledged"
  | "intent_acknowledgement_failed"
  | "intent_handler_failed";

export interface CaptureIntentDiagnostic {
  code: CaptureIntentDiagnosticCode;
  intentId?: number;
  action?: CaptureIntent["kind"];
}

const tauriPort: CaptureIntentConsumerPort = {
  getPendingCaptureIntent,
  acknowledgeCaptureIntent,
};

/**
 * Serializes durable command delivery for the target window. Reading an
 * intent never removes it; only successful handling followed by an explicit
 * acknowledgement advances the queue.
 */
export class CaptureIntentConsumer {
  private readonly port: CaptureIntentConsumerPort;
  private diagnose?: (event: CaptureIntentDiagnostic) => void;
  private inFlight: Promise<void> | null = null;
  private readonly target: "conversation" | "settings";

  constructor(
    port: CaptureIntentConsumerPort = tauriPort,
    diagnose?: (event: CaptureIntentDiagnostic) => void,
    target: "conversation" | "settings" = "conversation",
  ) {
    this.port = port;
    this.diagnose = diagnose;
    this.target = target;
  }

  setDiagnosticListener(diagnose?: (event: CaptureIntentDiagnostic) => void): void {
    this.diagnose = diagnose;
  }

  consume(
    targetReady: boolean,
    handle: (intent: CaptureIntent) => Promise<void>,
  ): Promise<void> {
    if (!targetReady) {
      this.diagnose?.({ code: "target_not_ready" });
      return Promise.resolve();
    }
    if (this.inFlight) return this.inFlight;

    const run = this.drain(handle).finally(() => {
      if (this.inFlight === run) this.inFlight = null;
    });
    this.inFlight = run;
    return run;
  }

  private async drain(handle: (intent: CaptureIntent) => Promise<void>): Promise<void> {
    while (true) {
      const intent = await this.port.getPendingCaptureIntent(this.target);
      if (!intent) return;
      this.diagnose?.({
        code: "target_ready",
        intentId: intent.id,
        action: intent.kind,
      });

      try {
        await handle(intent);
      } catch (error) {
        this.diagnose?.({
          code: "intent_handler_failed",
          intentId: intent.id,
          action: intent.kind,
        });
        throw error;
      }

      const acknowledged = await this.port.acknowledgeCaptureIntent(intent.id);
      if (!acknowledged) {
        this.diagnose?.({
          code: "intent_acknowledgement_failed",
          intentId: intent.id,
          action: intent.kind,
        });
        throw new Error("capture_intent_acknowledgement_failed");
      }
      this.diagnose?.({
        code: "intent_acknowledged",
        intentId: intent.id,
        action: intent.kind,
      });
    }
  }
}

// One consumer per WebView module survives React StrictMode remounts and
// prevents two listener generations from executing the same pending intent.
export const captureIntentConsumer = new CaptureIntentConsumer();
export const settingsIntentConsumer = new CaptureIntentConsumer(tauriPort, undefined, "settings");
