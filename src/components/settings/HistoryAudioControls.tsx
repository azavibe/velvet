import { AlertTriangle, LoaderCircle, Pause, Play, RotateCcw, Square } from "lucide-react";
import type { TFunction } from "i18next";
import { useTranslation } from "react-i18next";
import { getAudioAssetUrl, type AudioAsset } from "@/services/tauriApi";
import type { AudioPlaybackRequest } from "@/services/audioPlayback";
import { useAudioPlayback } from "@/services/useAudioPlayback";
import { resolveHistoryAudioSources } from "@/services/historyAudioSources";
import { Button } from "@/components/ui/button";

export type HistoryPlayback = ReturnType<typeof useAudioPlayback>;

interface HistoryAudioControlsProps {
  itemKey: string;
  playback: HistoryPlayback;
  dictation?: AudioAsset | null;
  conversation?: {
    me: AudioAsset | null;
    them: AudioAsset | null;
  };
  noteSegments?: AudioAsset[];
}

function formatAudioTime(seconds: number | null): string {
  if (seconds == null || !Number.isFinite(seconds) || seconds < 0) return "0:00";
  const whole = Math.floor(seconds);
  const minutes = Math.floor(whole / 60);
  const remaining = String(whole % 60).padStart(2, "0");
  return `${minutes}:${remaining}`;
}

function sourceRequest(
  key: string,
  assets: AudioAsset[],
  skipUnavailable = false,
): AudioPlaybackRequest {
  return {
    key,
    resolve: () => resolveHistoryAudioSources(assets, getAudioAssetUrl, skipUnavailable),
  };
}

function statusText(t: TFunction, status: AudioAsset["status"]): string {
  switch (status) {
    case "saving":
      return t("history.audio.saving");
    case "missing":
      return t("history.audio.missing");
    case "failed":
      return t("history.audio.failed");
    case "ready":
      return "";
  }
}

function playbackErrorText(t: TFunction, error: string | null): string {
  if (error === "audio_missing" || error === "audio_not_found") return t("history.audio.deleted");
  if (error === "audio_unsupported") return t("history.audio.unsupported");
  return t("history.audio.playbackFailed");
}

function AudioTimeline({ playback }: { playback: HistoryPlayback }) {
  const { t } = useTranslation();
  if (playback.key == null || playback.duration == null || playback.duration <= 0) return null;
  const current = Math.min(Math.max(0, playback.currentTime), playback.duration);
  return (
    <div className="flex items-center gap-2 text-xs text-muted-foreground">
      <input
        type="range"
        min={0}
        max={playback.duration}
        step={0.1}
        value={current}
        onChange={(event) => playback.seek(Number(event.target.value))}
        aria-label={t("history.audio.seek")}
        className="min-w-0 flex-1 accent-accent"
      />
      <span className="tabular-nums whitespace-nowrap" aria-live="off">
        {t("history.audio.duration", {
          current: formatAudioTime(current),
          duration: formatAudioTime(playback.duration),
        })}
      </span>
    </div>
  );
}

function AudioTrack({
  asset,
  trackKey,
  label,
  playback,
}: {
  asset: AudioAsset;
  trackKey: string;
  label: string;
  playback: HistoryPlayback;
}) {
  const { t } = useTranslation();
  const active = playback.key === trackKey;
  const playable = asset.status === "ready";
  const activeInProgress = active && ["loading", "playing", "paused"].includes(playback.status);
  const actionLabel = active && playback.status === "playing"
    ? t("history.audio.pause")
    : active && playback.status === "paused"
      ? t("history.audio.resume")
      : t("history.audio.play");

  const play = () => playback.toggle(sourceRequest(trackKey, [asset]));

  return (
    <div className="rounded-control bg-surface-2 px-2.5 py-2 space-y-1.5" role="group" aria-label={label}>
      <div className="flex items-center gap-2 min-w-0">
        <span className="text-sm text-foreground truncate flex-1">{label}</span>
        {!playable && <span className="text-xs text-muted-foreground">{statusText(t, asset.status)}</span>}
        <Button
          variant="ghost"
          size="sm"
          onClick={() => void play()}
          disabled={!playable}
          aria-label={`${actionLabel}: ${label}`}
          title={`${actionLabel}: ${label}`}
        >
          {active && playback.status === "loading" ? (
            <LoaderCircle className="w-3.5 h-3.5 animate-spin" />
          ) : active && playback.status === "playing" ? (
            <Pause className="w-3.5 h-3.5" />
          ) : (
            <Play className="w-3.5 h-3.5" />
          )}
          {actionLabel}
        </Button>
        {activeInProgress && (
          <>
            <Button
              variant="ghost"
              size="sm"
              onClick={() => void playback.restart()}
              aria-label={`${t("history.audio.restart")}: ${label}`}
              title={`${t("history.audio.restart")}: ${label}`}
            >
              <RotateCcw className="w-3.5 h-3.5" />
              {t("history.audio.restart")}
            </Button>
            <Button
              variant="ghost"
              size="sm"
              onClick={playback.stop}
              aria-label={`${t("history.audio.stop")}: ${label}`}
              title={`${t("history.audio.stop")}: ${label}`}
            >
              <Square className="w-3.5 h-3.5" />
              {t("history.audio.stop")}
            </Button>
          </>
        )}
      </div>
      {active && playback.status === "error" && (
        <p className="flex items-center gap-1 text-xs text-destructive" role="alert">
          <AlertTriangle className="w-3.5 h-3.5 shrink-0" />
          {playbackErrorText(t, playback.error)}
        </p>
      )}
      {active && <AudioTimeline playback={playback} />}
    </div>
  );
}

function NoteAudioQueue({ itemKey, segments, playback }: { itemKey: string; segments: AudioAsset[]; playback: HistoryPlayback }) {
  const { t } = useTranslation();
  const orderedSegments = [...segments].sort((a, b) => a.sequence - b.sequence || a.id - b.id);
  const readySegments = orderedSegments.filter((segment) => segment.status === "ready");
  const queueKey = `${itemKey}:note-all`;
  const active = playback.key === queueKey;
  const activeInProgress = active && ["loading", "playing", "paused"].includes(playback.status);
  const playAll = () => playback.toggle(sourceRequest(queueKey, readySegments, true));

  return (
    <div className="space-y-1.5">
      <div className="rounded-control bg-surface-2 px-2.5 py-2 space-y-1.5" role="group" aria-label={t("history.audio.noteSegments")}>
        <div className="flex items-center gap-2">
          <span className="text-sm text-foreground flex-1">{t("history.audio.noteSegments")}</span>
          <Button
            variant="ghost"
            size="sm"
            onClick={() => void playAll()}
            disabled={readySegments.length === 0}
            aria-label={t("history.audio.playAll")}
            title={t("history.audio.playAll")}
          >
            {active && playback.status === "loading" ? (
              <LoaderCircle className="w-3.5 h-3.5 animate-spin" />
            ) : active && playback.status === "playing" ? (
              <Pause className="w-3.5 h-3.5" />
            ) : (
              <Play className="w-3.5 h-3.5" />
            )}
            {t("history.audio.playAll")}
          </Button>
          {activeInProgress && (
            <Button
              variant="ghost"
              size="sm"
              onClick={playback.stop}
              aria-label={t("history.audio.stop")}
              title={t("history.audio.stop")}
            >
              <Square className="w-3.5 h-3.5" />
              {t("history.audio.stop")}
            </Button>
          )}
        </div>
        {active && playback.queueLength > 0 && (
          <p className="text-xs text-muted-foreground">
            {t("history.audio.segment", {
              current: Math.min(playback.queueIndex + 1, playback.queueLength),
              total: playback.queueLength,
            })}
          </p>
        )}
        {active && playback.status === "error" && (
          <p className="flex items-center gap-1 text-xs text-destructive" role="alert">
            <AlertTriangle className="w-3.5 h-3.5 shrink-0" />
            {playbackErrorText(t, playback.error)}
          </p>
        )}
        {active && <AudioTimeline playback={playback} />}
        {readySegments.length === 0 && <p className="text-xs text-muted-foreground">{t("history.audio.noReadySegments")}</p>}
      </div>
      {orderedSegments.map((segment, index) => (
        <AudioTrack
          key={segment.id}
          asset={segment}
          trackKey={`${itemKey}:note-segment:${segment.id}`}
          label={t("history.audio.segment", { current: index + 1, total: orderedSegments.length })}
          playback={playback}
        />
      ))}
    </div>
  );
}

export function HistoryAudioControls({
  itemKey,
  playback,
  dictation,
  conversation,
  noteSegments,
}: HistoryAudioControlsProps) {
  const { t } = useTranslation();
  const hasAudio = Boolean(dictation || conversation?.me || conversation?.them || noteSegments?.length);
  if (!hasAudio) return null;

  return (
    <section className="space-y-2" aria-labelledby={`${itemKey}-audio-heading`}>
      <h3 id={`${itemKey}-audio-heading`} className="text-xs uppercase tracking-wider text-muted-foreground/80">
        {t("history.audio.title")}
      </h3>
      {dictation && (
        <AudioTrack
          asset={dictation}
          trackKey={`${itemKey}:dictation`}
          label={t("history.typeDictation")}
          playback={playback}
        />
      )}
      {conversation && (
        <div className="space-y-1.5">
          {conversation.me && (
            <AudioTrack
              asset={conversation.me}
              trackKey={`${itemKey}:me`}
              label={t("history.audio.me")}
              playback={playback}
            />
          )}
          {conversation.them && (
            <AudioTrack
              asset={conversation.them}
              trackKey={`${itemKey}:them`}
              label={t("history.audio.them")}
              playback={playback}
            />
          )}
        </div>
      )}
      {noteSegments && noteSegments.length > 0 && (
        <NoteAudioQueue itemKey={itemKey} segments={noteSegments} playback={playback} />
      )}
    </section>
  );
}
