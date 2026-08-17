import { useEffect, useState } from "react";
import { useTranslation } from "react-i18next";
import { getSetting, setSetting } from "@/services/tauriApi";

interface ConversationConsentModalProps {
  /** Rendered only while this is true — caller controls exactly when consent
   *  is being asked for (e.g. right before starting a conversation). */
  requested: boolean;
  onAccept: () => void;
  onCancel: () => void;
}

/**
 * Recording other people is regulated in many places (two-party-consent
 * states, GDPR) — this gate exists so starting a conversation is a
 * deliberate choice, not an accidental one, same as Live dictation's
 * LiveConsentModal. Settings store: `conversationConsent` boolean.
 */
export function ConversationConsentModal({ requested, onAccept, onCancel }: ConversationConsentModalProps) {
  const { t } = useTranslation();
  const [show, setShow] = useState(false);

  useEffect(() => {
    let cancelled = false;
    if (!requested) {
      setShow(false);
      return;
    }
    (async () => {
      const consented = await getSetting<boolean>("conversationConsent");
      if (cancelled) return;
      if (consented) {
        onAccept();
      } else {
        setShow(true);
      }
    })();
    return () => {
      cancelled = true;
    };
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [requested]);

  async function accept() {
    await setSetting("conversationConsent", true);
    setShow(false);
    onAccept();
  }

  function cancel() {
    setShow(false);
    onCancel();
  }

  if (!show) return null;

  return (
    <div className="fixed inset-0 bg-black/50 flex items-center justify-center z-50">
      <div className="bg-background border border-border rounded-control p-6 max-w-md space-y-4">
        <h2 className="text-lg font-semibold">{t("conversation.consent.title")}</h2>
        <p className="text-sm text-muted-foreground leading-relaxed">{t("conversation.consent.body")}</p>
        <div className="flex gap-2 justify-end">
          <button onClick={cancel} className="px-4 py-2 rounded-control text-muted-foreground hover:bg-surface-1">
            {t("conversation.consent.cancel")}
          </button>
          <button
            onClick={accept}
            className="px-4 py-2 rounded-control bg-primary text-primary-foreground hover:bg-primary/90"
          >
            {t("conversation.consent.confirm")}
          </button>
        </div>
      </div>
    </div>
  );
}
