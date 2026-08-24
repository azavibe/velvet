import { useEffect, useRef } from "react";
import { register, unregister } from "@tauri-apps/plugin-global-shortcut";
import {
  HotkeyRegistrationController,
  type HotkeyEvent,
} from "@/services/hotkeyLifecycle";
import { startupMark } from "@/services/startupDiagnostics";

interface UseHotkeyOptions {
  shortcut: string;
  activationMode: "tap" | "push";
  onToggle: () => void;
  onPushStart?: () => void;
  onPushEnd?: () => void;
  onRegistrationError?: () => void;
  enabled?: boolean;
}

export function useHotkey({
  shortcut,
  activationMode,
  onToggle,
  onPushStart,
  onPushEnd,
  onRegistrationError,
  enabled = true,
}: UseHotkeyOptions) {
  const onToggleRef = useRef(onToggle);
  const onPushStartRef = useRef(onPushStart);
  const onPushEndRef = useRef(onPushEnd);
  const onRegistrationErrorRef = useRef(onRegistrationError);
  const activationModeRef = useRef(activationMode);
  const controllerRef = useRef<HotkeyRegistrationController | null>(null);

  onToggleRef.current = onToggle;
  onPushStartRef.current = onPushStart;
  onPushEndRef.current = onPushEnd;
  onRegistrationErrorRef.current = onRegistrationError;
  activationModeRef.current = activationMode;

  if (!controllerRef.current) {
    controllerRef.current = new HotkeyRegistrationController({
      register: (key, callback) => register(key, callback),
      unregister,
      onFailure: (phase) => {
        startupMark("global shortcut lifecycle failure", { phase });
        onRegistrationErrorRef.current?.();
      },
      onRegistered: () => startupMark("global shortcut registered"),
    });
  }

  const controller = controllerRef.current;

  useEffect(() => {
    const callback = (event: HotkeyEvent) => {
      if (activationModeRef.current === "tap") {
        if (event.state === "Pressed") onToggleRef.current();
      } else if (event.state === "Pressed") {
        onPushStartRef.current?.();
      } else if (event.state === "Released") {
        onPushEndRef.current?.();
      }
    };

    startupMark("global shortcut registration requested", {
      enabled,
      configured: !!shortcut,
    });
    void controller.update(shortcut, enabled, callback);

    return () => {
      void controller.disable();
    };
  }, [controller, shortcut, enabled]);

  // Re-register hotkey when the window regains focus (e.g. after a remote
  // desktop session like RustDesk disrupts OS-level global hotkey hooks).
  useEffect(() => {
    if (!shortcut || !enabled) return;

    const handleFocus = () => {
      void controller.retry();
    };

    window.addEventListener("focus", handleFocus);
    return () => window.removeEventListener("focus", handleFocus);
  }, [controller, shortcut, enabled]);
}
