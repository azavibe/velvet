import React, { useEffect, useMemo, useRef, useState } from "react";
import { useTranslation } from "react-i18next";
import { Copy, Trash2, ChevronDown, Mic, MessagesSquare, NotebookPen, Pencil, Sparkles, Mic2, RefreshCw, AlertTriangle, FolderOpen } from "lucide-react";
import {
  getStats,
  getTranscriptions,
  deleteTranscription,
  listConversations,
  getConversation,
  deleteConversation,
  listNotes,
  updateNote,
  deleteNote,
  cleanupNote,
  requestNoteAppend,
  showConversationWindow,
  setClipboardText,
  onAudioRecoveryComplete,
  onAudioRecoveryFailed,
  retryFailedTranscription,
  retranscribeLocal,
  type StatsPayload,
  type Transcription,
  type ConversationSummary,
  type Note,
} from "@/services/tauriApi";
import { SettingsSection } from "@/components/ui/SettingsSection";
import { Button } from "@/components/ui/button";
import type { Settings } from "@/hooks/useSettings";
import { HistoryAudioControls } from "@/components/settings/HistoryAudioControls";
import { audioPlaybackController } from "@/services/audioPlayback";
import { useAudioPlayback } from "@/services/useAudioPlayback";
import { startupMark } from "@/services/startupDiagnostics";
import { mergeUniqueById, uniqueHistoryItems } from "@/components/settings/historyList";
import { buildTranscriptionDictionary } from "@/hooks/useTranscriptionPipeline";
import { protectedDictionaryTerms } from "@/models/dictionary";
import { extractAndStoreMemory } from "@/services/memory";

type Loaded = { today: StatsPayload; week: StatsPayload; all: StatsPayload };

type ToastFn = (props: { title?: string; description?: string; variant: "default" | "destructive" | "success" }) => void;

// One page of each source per "Load more" click — small enough to keep the
// merged list's memory footprint well under the ~10MB budget even with a
// few pages loaded, since only summaries (not full conversation transcripts)
// are fetched up front.
const PAGE_SIZE = 20;

type HistoryItem =
  | { kind: "dictation"; id: string; timestamp: string; data: Transcription }
  | { kind: "conversation"; id: string; timestamp: string; data: ConversationSummary }
  | { kind: "note"; id: string; timestamp: string; data: Note };

function formatDuration(seconds: number): string {
  if (!Number.isFinite(seconds) || seconds < 1) return "0s";
  const s = Math.round(seconds);
  if (s < 60) return `${s}s`;
  const m = Math.floor(s / 60);
  const rs = s % 60;
  if (m < 60) return rs > 0 ? `${m}m ${rs}s` : `${m}m`;
  const h = Math.floor(m / 60);
  const rm = m % 60;
  return rm > 0 ? `${h}h ${rm}m` : `${h}h`;
}

function formatCount(n: number): string {
  return Math.round(n).toLocaleString();
}

function conversationDurationSeconds(c: ConversationSummary): number | null {
  if (!c.ended_at) return null;
  const start = Date.parse(c.started_at);
  const end = Date.parse(c.ended_at);
  if (Number.isNaN(start) || Number.isNaN(end) || end < start) return null;
  return (end - start) / 1000;
}

function snippetLines(text: string): string {
  // Collapse to at most two lines for the collapsed card preview.
  const lines = text.trim().split(/\r?\n/).filter(Boolean);
  return lines.slice(0, 2).join("\n");
}

/** The cleanup pass always opens with "# Title" — strip it from the body
 *  preview/expanded view since the title already renders separately. */
function stripMarkdownTitle(markdown: string): string {
  return markdown.replace(/^#\s+.*\n?/, "").trim();
}

function noteTitleFrom(note: Note): string | null {
  if (note.title) return note.title;
  const match = note.body_markdown?.match(/^#\s+(.+)$/m);
  return match ? match[1].trim() : null;
}

/** Minimal, dependency-free renderer for the small Markdown subset the
 *  cleanup prompt produces (bullets, **bold**, headings) — not a general
 *  Markdown parser, just enough for AI-cleaned note bodies. */
function MarkdownLite({ text }: { text: string }) {
  const renderInline = (line: string, key: number) => {
    const parts = line.split(/(\*\*[^*]+\*\*)/g);
    return (
      <p key={key} className="text-sm text-foreground leading-relaxed">
        {parts.map((part, i) =>
          part.startsWith("**") && part.endsWith("**") ? (
            <strong key={i} className="font-semibold text-foreground-bright">
              {part.slice(2, -2)}
            </strong>
          ) : (
            <span key={i}>{part}</span>
          ),
        )}
      </p>
    );
  };

  const lines = text.split(/\r?\n/);
  const nodes: React.ReactNode[] = [];
  let listBuffer: string[] = [];
  const flushList = (key: string) => {
    if (listBuffer.length === 0) return;
    nodes.push(
      <ul key={key} className="list-disc list-inside space-y-0.5">
        {listBuffer.map((item, i) => (
          <li key={i} className="text-sm text-foreground leading-relaxed">
            {item}
          </li>
        ))}
      </ul>,
    );
    listBuffer = [];
  };

  lines.forEach((line, idx) => {
    const trimmed = line.trim();
    if (trimmed.startsWith("# ")) {
      flushList(`list-${idx}`);
      nodes.push(
        <p key={idx} className="text-sm font-semibold text-foreground-bright">
          {trimmed.slice(2)}
        </p>,
      );
    } else if (trimmed.startsWith("- ") || trimmed.startsWith("* ")) {
      listBuffer.push(trimmed.slice(2));
    } else if (trimmed) {
      flushList(`list-${idx}`);
      nodes.push(renderInline(trimmed, idx));
    }
  });
  flushList("list-end");

  return <div className="space-y-1.5">{nodes}</div>;
}

export default function StatisticsSection({ settings, toast }: { settings: Settings; toast?: ToastFn }) {
  const { t } = useTranslation();
  const playback = useAudioPlayback();
  const [stats, setStats] = useState<Loaded | null>(null);
  const [statsError, setStatsError] = useState<string | null>(null);

  const [transcriptions, setTranscriptions] = useState<Transcription[]>([]);
  const [conversations, setConversations] = useState<ConversationSummary[]>([]);
  const [notes, setNotes] = useState<Note[]>([]);
  const [tOffset, setTOffset] = useState(0);
  const [cOffset, setCOffset] = useState(0);
  const [nOffset, setNOffset] = useState(0);
  const [hasMoreT, setHasMoreT] = useState(true);
  const [hasMoreC, setHasMoreC] = useState(true);
  const [hasMoreN, setHasMoreN] = useState(true);
  const [loadingMore, setLoadingMore] = useState(false);
  const [listError, setListError] = useState<string | null>(null);
  const [historyRefreshToken, setHistoryRefreshToken] = useState(0);
  const firstHistoryQueryRef = useRef(false);
  const loadInFlightRef = useRef(false);
  const queuedReloadRef = useRef(false);
  const historyRequestGenerationRef = useRef(0);
  const mountedRef = useRef(true);
  const loadMoreRef = useRef<() => Promise<void>>(() => Promise.resolve());

  const [expandedId, setExpandedId] = useState<string | null>(null);
  const [expandedDetail, setExpandedDetail] = useState<Record<string, string>>({});
  const [confirmDeleteId, setConfirmDeleteId] = useState<string | null>(null);
  const [editingId, setEditingId] = useState<string | null>(null);
  const [editDraft, setEditDraft] = useState<{ title: string; transcript: string }>({ title: "", transcript: "" });
  const [cleaningUpId, setCleaningUpId] = useState<string | null>(null);
  const [retryingId, setRetryingId] = useState<string | null>(null);

  useEffect(() => () => audioPlaybackController.stop(), []);

  useEffect(() => {
    mountedRef.current = true;
    return () => {
      mountedRef.current = false;
    };
  }, []);

  useEffect(() => {
    let cancelled = false;
    (async () => {
      try {
        const [today, week, all] = await Promise.all([
          getStats("today"),
          getStats("week"),
          getStats("all"),
        ]);
        if (!cancelled) setStats({ today, week, all });
      } catch (e) {
        if (!cancelled) setStatsError(e instanceof Error ? e.message : String(e));
      }
    })();
    return () => {
      cancelled = true;
    };
  }, []);

  useEffect(() => {
    const cancelledForRecovery = { current: false };
    const unlistenComplete = onAudioRecoveryComplete((report) => {
      if (cancelledForRecovery.current) return;
      startupMark("audio recovery event received", {
        checked: report.checked,
        ready: report.ready,
        missing: report.missing,
        failed: report.failed,
      });
      audioPlaybackController.stop();
      historyRequestGenerationRef.current += 1;
      if (loadInFlightRef.current) queuedReloadRef.current = true;
      setTranscriptions([]);
      setConversations([]);
      setNotes([]);
      setTOffset(0);
      setCOffset(0);
      setNOffset(0);
      setHasMoreT(true);
      setHasMoreC(true);
      setHasMoreN(true);
      setListError(null);
      setHistoryRefreshToken((token) => token + 1);
    });
    const unlistenFailed = onAudioRecoveryFailed(() => {
      if (cancelledForRecovery.current) return;
      startupMark("audio recovery event failed");
      setListError(t("history.audioRecoveryFailed"));
    });
    return () => {
      cancelledForRecovery.current = true;
      unlistenComplete.then((unlisten) => unlisten());
      unlistenFailed.then((unlisten) => unlisten());
    };
  }, [t]);

  const loadMore = async () => {
    if (loadInFlightRef.current) {
      // React StrictMode can invoke the initial effect twice. Ignore that
      // second request; only a recovery-triggered reload is queued.
      if (historyRefreshToken > 0) queuedReloadRef.current = true;
      return;
    }
    loadInFlightRef.current = true;
    const requestGeneration = historyRequestGenerationRef.current;
    const requestedOffsets = { t: tOffset, c: cOffset, n: nOffset };
    const requestedMore = { t: hasMoreT, c: hasMoreC, n: hasMoreN };
    if (!firstHistoryQueryRef.current) {
      firstHistoryQueryRef.current = true;
      startupMark("first History/Notes query started");
    }
    setLoadingMore(true);
    setListError(null);
    try {
      const [nextT, nextC, nextN] = await Promise.all([
        requestedMore.t ? getTranscriptions(PAGE_SIZE, requestedOffsets.t) : Promise.resolve([]),
        requestedMore.c ? listConversations(PAGE_SIZE, requestedOffsets.c) : Promise.resolve([]),
        requestedMore.n ? listNotes(PAGE_SIZE, requestedOffsets.n) : Promise.resolve([]),
      ]);
      if (requestGeneration !== historyRequestGenerationRef.current) return;
      if (nextT.length > 0) {
        setTranscriptions((prev) => mergeUniqueById(prev, nextT));
        setTOffset((o) => o + nextT.length);
      }
      if (nextT.length < PAGE_SIZE) setHasMoreT(false);
      if (nextC.length > 0) {
        setConversations((prev) => mergeUniqueById(prev, nextC));
        setCOffset((o) => o + nextC.length);
      }
      if (nextC.length < PAGE_SIZE) setHasMoreC(false);
      if (nextN.length > 0) {
        setNotes((prev) => mergeUniqueById(prev, nextN));
        setNOffset((o) => o + nextN.length);
      }
      if (nextN.length < PAGE_SIZE) setHasMoreN(false);
      startupMark("History/Notes query complete", {
        dictations: nextT.length,
        conversations: nextC.length,
        notes: nextN.length,
      });
    } catch (e) {
      if (requestGeneration === historyRequestGenerationRef.current) {
        setListError(e instanceof Error ? e.message : String(e));
      }
    } finally {
      loadInFlightRef.current = false;
      setLoadingMore(false);
      if (queuedReloadRef.current) {
        queuedReloadRef.current = false;
        window.setTimeout(() => {
          if (mountedRef.current) void loadMoreRef.current();
        }, 0);
      }
    }
  };

  loadMoreRef.current = loadMore;

  useEffect(() => {
    loadMore();
    // The recovery event increments the token after clearing the old page.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [historyRefreshToken]);

  const items = useMemo<HistoryItem[]>(() => {
    const merged: HistoryItem[] = [
      ...transcriptions.map((tr) => ({
        kind: "dictation" as const,
        id: `d-${tr.id}`,
        timestamp: tr.timestamp,
        data: tr,
      })),
      ...conversations.map((c) => ({
        kind: "conversation" as const,
        id: `c-${c.id}`,
        timestamp: c.started_at,
        data: c,
      })),
      ...notes.map((n) => ({
        kind: "note" as const,
        id: `n-${n.id}`,
        timestamp: n.updated_at,
        data: n,
      })),
    ];
    merged.sort((a, b) => Date.parse(b.timestamp) - Date.parse(a.timestamp));
    return uniqueHistoryItems(merged);
  }, [transcriptions, conversations, notes]);

  const toggleExpand = async (item: HistoryItem) => {
    if (expandedId === item.id) {
      playback.stop();
      setExpandedId(null);
      return;
    }
    playback.stop();
    setExpandedId(item.id);
    setEditingId(null);
    if (item.kind === "conversation" && !(item.id in expandedDetail)) {
      try {
        const detail = await getConversation(item.data.id);
        const text = detail.utterances
          .map((u) => `${u.channel === "me" ? t("history.speakerMe") : t("history.speakerThem")}: ${u.text}`)
          .join("\n");
        setExpandedDetail((prev) => ({ ...prev, [item.id]: text || t("history.emptyTranscript") }));
      } catch (e) {
        setExpandedDetail((prev) => ({ ...prev, [item.id]: String(e) }));
      }
    }
  };

  const fullText = (item: HistoryItem): string => {
    if (item.kind === "dictation") {
      return item.data.processed_text || item.data.original_text;
    }
    if (item.kind === "note") {
      return item.data.body_markdown ? stripMarkdownTitle(item.data.body_markdown) : item.data.raw_transcript;
    }
    return expandedDetail[item.id] ?? item.data.snippet ?? "";
  };

  const handleCopy = async (item: HistoryItem) => {
    try {
      await setClipboardText(fullText(item));
      toast?.({ title: t("history.copied"), variant: "success" });
    } catch (e) {
      toast?.({ title: t("history.copyFailed"), description: String(e), variant: "destructive" });
    }
  };

  const handleDelete = async (item: HistoryItem) => {
    try {
      if (playback.key?.startsWith(`${item.id}:`)) playback.stop();
      if (item.kind === "dictation") {
        await deleteTranscription(item.data.id);
        setTranscriptions((prev) => prev.filter((tr) => tr.id !== item.data.id));
      } else if (item.kind === "note") {
        await deleteNote(item.data.id);
        setNotes((prev) => prev.filter((n) => n.id !== item.data.id));
      } else {
        await deleteConversation(item.data.id);
        setConversations((prev) => prev.filter((c) => c.id !== item.data.id));
      }
      if (expandedId === item.id) setExpandedId(null);
      setConfirmDeleteId(null);
    } catch (e) {
      toast?.({ title: t("history.deleteFailed"), description: String(e), variant: "destructive" });
    }
  };

  const startEditingNote = (item: Extract<HistoryItem, { kind: "note" }>) => {
    setEditingId(item.id);
    setEditDraft({ title: noteTitleFrom(item.data) ?? "", transcript: item.data.raw_transcript });
  };

  const saveNoteEdit = async (item: Extract<HistoryItem, { kind: "note" }>) => {
    try {
      await updateNote(item.data.id, editDraft.title.trim() || null, editDraft.transcript);
      setNotes((prev) =>
        prev.map((n) =>
          n.id === item.data.id
            ? { ...n, title: editDraft.title.trim() || null, raw_transcript: editDraft.transcript, updated_at: new Date().toISOString() }
            : n,
        ),
      );
      setEditingId(null);
    } catch (e) {
      toast?.({ title: t("notes.saveFailed"), description: String(e), variant: "destructive" });
    }
  };

  const handleAppendDictation = async (item: Extract<HistoryItem, { kind: "note" }>) => {
    try {
      await showConversationWindow();
      await requestNoteAppend(item.data.id);
    } catch (e) {
      toast?.({ title: t("notes.appendFailed"), description: String(e), variant: "destructive" });
    }
  };

  const handleCleanupNote = async (item: Extract<HistoryItem, { kind: "note" }>) => {
    const provider = settings.conversationReasoningProvider;
    const apiKey = (settings[`${provider}ApiKey` as keyof Settings] as string) ?? "";
    if (!apiKey) {
      toast?.({ title: t("notes.cleanupNoKey"), variant: "destructive" });
      return;
    }
    setCleaningUpId(item.id);
    try {
      const body = await cleanupNote(item.data.id, settings.conversationReasoningModel, provider, apiKey);
      setNotes((prev) => prev.map((n) => (n.id === item.data.id ? { ...n, body_markdown: body } : n)));
    } catch (e) {
      toast?.({ title: t("notes.cleanupFailed"), description: String(e), variant: "destructive" });
    } finally {
      setCleaningUpId(null);
    }
  };

  const recordingPaths = (item: HistoryItem): string[] => {
    if (item.kind === "dictation") {
      return item.data.audio_asset?.path ? [item.data.audio_asset.path] : [];
    }
    if (item.kind === "conversation") {
      return [item.data.audio_asset_me?.path, item.data.audio_asset_them?.path].filter(
        (path): path is string => Boolean(path),
      );
    }
    return item.data.audio_segments
      .map((asset) => asset.path)
      .filter((path): path is string => Boolean(path));
  };

  const handleOpenRecordingFolder = async (item: HistoryItem) => {
    const paths = recordingPaths(item);
    if (paths.length === 0) {
      toast?.({ title: t("history.openFolderNoAudio"), variant: "destructive" });
      return;
    }
    try {
      const { revealItemInDir } = await import("@tauri-apps/plugin-opener");
      await revealItemInDir(paths);
    } catch (e) {
      toast?.({ title: t("history.openFolderFailed"), description: String(e), variant: "destructive" });
    }
  };

  const handleRetryTranscription = async (
    item: Extract<HistoryItem, { kind: "dictation" }>,
  ) => {
    const asset = item.data.audio_asset;
    if (!asset || asset.status !== "ready") {
      toast?.({ title: t("history.retryNoAudio"), variant: "destructive" });
      return;
    }
    const provider = settings.cloudTranscriptionProvider;
    const apiKey = (settings[`${provider}ApiKey` as keyof Settings] as string) ?? "";
    if (!apiKey) {
      toast?.({ title: t("history.retryNoKey"), variant: "destructive" });
      return;
    }
    const dictionary = buildTranscriptionDictionary(
      settings.customDictionary,
      settings.agentName,
      settings.agentAliases,
    );
    const protectedTerms = [
      settings.agentName,
      ...settings.agentAliases,
      ...protectedDictionaryTerms(settings.customDictionary),
    ].filter((term) => term.trim());

    setRetryingId(item.id);
    try {
      const result = await retryFailedTranscription({
        transcriptionId: item.data.id,
        audioAssetId: asset.id,
        provider,
        apiKey,
        model: settings.cloudTranscriptionModel,
        language: settings.languageMode === "auto" ? "auto" : settings.preferredLanguage,
        secondaryLanguage:
          settings.languageMode === "bilingual"
            ? settings.secondaryLanguage || undefined
            : undefined,
        dictionary,
        protectedTerms,
      });
      setTranscriptions((previous) => previous.map((transcription) =>
        transcription.id === item.data.id
          ? {
              ...transcription,
              original_text: result.text,
              processed_text: null,
              error: null,
              processing_method: `retry:${provider}`,
              word_count: result.text.trim().split(/\s+/u).filter(Boolean).length,
            }
          : transcription,
      ));
      const [today, week, all] = await Promise.all([
        getStats("today"),
        getStats("week"),
        getStats("all"),
      ]);
      setStats({ today, week, all });
      if (settings.automaticMemoryEnabled) {
        void extractAndStoreMemory(result.text, "dictation", item.data.id, settings);
      }
      toast?.({ title: t("history.retrySuccess"), variant: "success" });
    } catch (e) {
      toast?.({
        title: t("history.retryFailed"),
        description: String(e),
        variant: "destructive",
      });
    } finally {
      setRetryingId(null);
    }
  };

  const handleLocalRetranscription = async (
    item: Extract<HistoryItem, { kind: "dictation" }>,
  ) => {
    const asset = item.data.audio_asset;
    if (!asset || asset.status !== "ready") {
      toast?.({ title: t("history.retryNoAudio"), variant: "destructive" });
      return;
    }
    if (!settings.localTranscriptionModel) {
      toast?.({
        title: t("history.localModelRequired"),
        variant: "destructive",
      });
      return;
    }
    const dictionary = buildTranscriptionDictionary(
      settings.customDictionary,
      settings.agentName,
      settings.agentAliases,
    );
    const protectedTerms = [
      settings.agentName,
      ...settings.agentAliases,
      ...protectedDictionaryTerms(settings.customDictionary),
    ].filter((term) => term.trim());
    setRetryingId(item.id);
    try {
      const result = await retranscribeLocal({
        transcriptionId: item.data.id,
        audioAssetId: asset.id,
        model: settings.localTranscriptionModel,
        language: settings.languageMode === "auto" ? "auto" : settings.preferredLanguage,
        secondaryLanguage:
          settings.languageMode === "bilingual" ? settings.secondaryLanguage || undefined : undefined,
        dictionary,
        protectedTerms,
      });
      setTranscriptions((previous) => previous.map((transcription) =>
        transcription.id === item.data.id
          ? {
              ...transcription,
              original_text: result.text,
              processed_text: null,
              error: null,
              processing_method: "retry:local",
              word_count: result.text.trim().split(/\s+/u).filter(Boolean).length,
            }
          : transcription,
      ));
      const [today, week, all] = await Promise.all([
        getStats("today"), getStats("week"), getStats("all"),
      ]);
      setStats({ today, week, all });
      if (settings.automaticMemoryEnabled) {
        void extractAndStoreMemory(result.text, "dictation", item.data.id, settings);
      }
      toast?.({ title: t("history.retrySuccess"), variant: "success" });
    } catch (e) {
      toast?.({ title: t("history.retryFailed"), description: String(e), variant: "destructive" });
    } finally {
      setRetryingId(null);
    }
  };

  return (
    <>
      {statsError ? (
        <SettingsSection title={t("stats.title")} description={t("stats.description")}>
          <p className="text-sm text-destructive">{statsError}</p>
        </SettingsSection>
      ) : !stats ? (
        <SettingsSection title={t("stats.title")} description={t("stats.description")}>
          <div className="grid grid-cols-2 gap-3">
            <div className="h-20 rounded-control bg-surface-1 animate-pulse" />
            <div className="h-20 rounded-control bg-surface-1 animate-pulse" />
          </div>
        </SettingsSection>
      ) : (
        <SettingsSection title={t("stats.title")} description={t("stats.description")}>
          <div className="grid grid-cols-2 gap-3">
            <div className="rounded-control bg-surface-1 px-4 py-3">
              <p className="text-2xl font-semibold text-foreground-bright tabular-nums">
                {formatDuration(stats.all.total_seconds)}
              </p>
              <p className="text-xs text-muted-foreground mt-0.5">{t("stats.totalAudio")}</p>
            </div>
            <div className="rounded-control bg-surface-1 px-4 py-3">
              <p className="text-2xl font-semibold text-foreground-bright tabular-nums">
                {formatCount(stats.all.total_words)}
              </p>
              <p className="text-xs text-muted-foreground mt-0.5">{t("stats.totalWords")}</p>
            </div>
          </div>

          <div className="pt-2">
            <p className="text-xs uppercase tracking-wider text-muted-foreground/80 mb-2">
              {t("stats.breakdown")}
            </p>
            <ul className="space-y-1.5 text-sm">
              <BreakdownRow label={t("stats.today")} periodStats={stats.today} />
              <BreakdownRow label={t("stats.thisWeek")} periodStats={stats.week} />
              <BreakdownRow label={t("stats.allTime")} periodStats={stats.all} />
            </ul>
          </div>

          <p className="pt-3 text-xs text-muted-foreground">
            {t("stats.average", {
              duration: formatDuration(stats.all.avg_seconds),
              words: formatCount(stats.all.avg_words),
            })}
          </p>
        </SettingsSection>
      )}

      <SettingsSection title={t("history.title")} description={t("history.description")}>
        {items.length === 0 && !loadingMore ? (
          <p className="text-sm text-muted-foreground">{t("history.empty")}</p>
        ) : (
          <div className="space-y-2">
            {items.map((item) => {
              const isExpanded = expandedId === item.id;
              const isConfirming = confirmDeleteId === item.id;
              const isEditing = editingId === item.id;
              const failedDictation = item.kind === "dictation" && item.data.error != null;
              const duration =
                item.kind === "dictation"
                  ? item.data.duration_ms != null
                    ? formatDuration(item.data.duration_ms / 1000)
                    : null
                  : item.kind === "conversation"
                    ? (() => {
                        const s = conversationDurationSeconds(item.data);
                        return s != null ? formatDuration(s) : null;
                      })()
                    : null;
              const preview =
                item.kind === "dictation"
                  ? failedDictation
                    ? item.data.error === "empty_transcription"
                      ? t("history.emptyRecording")
                      : t("history.transcriptionFailed")
                    : snippetLines(item.data.processed_text || item.data.original_text)
                  : item.kind === "note"
                    ? snippetLines(
                        (item.data.body_markdown ? stripMarkdownTitle(item.data.body_markdown) : item.data.raw_transcript) ||
                          t("history.emptyTranscript"),
                      )
                    : snippetLines(item.data.snippet || t("history.emptyTranscript"));
              const noteTitle = item.kind === "note" ? noteTitleFrom(item.data) : null;

              return (
                <div key={item.id} className="rounded-control bg-surface-1 overflow-hidden">
                  <button
                    type="button"
                    onClick={() => toggleExpand(item)}
                    className="w-full text-left px-3 py-2.5 flex items-start gap-3 hover:bg-surface-2 transition-colors"
                  >
                    {item.kind === "dictation" ? (
                      failedDictation ? (
                        <AlertTriangle className="w-4 h-4 mt-0.5 shrink-0 text-destructive" />
                      ) : (
                        <Mic className="w-4 h-4 mt-0.5 shrink-0 text-muted-foreground" />
                      )
                    ) : item.kind === "note" ? (
                      <NotebookPen className="w-4 h-4 mt-0.5 shrink-0 text-muted-foreground" />
                    ) : (
                      <MessagesSquare className="w-4 h-4 mt-0.5 shrink-0 text-muted-foreground" />
                    )}
                    <div className="min-w-0 flex-1">
                      <div className="flex items-center gap-2 text-xs text-muted-foreground">
                        <span>{new Date(item.timestamp).toLocaleString()}</span>
                        {duration && <span>· {duration}</span>}
                        <span>
                          ·{" "}
                          {item.kind === "dictation"
                            ? t("history.typeDictation")
                            : item.kind === "note"
                              ? t("history.typeNote")
                              : t("history.typeConversation")}
                        </span>
                        {failedDictation && (
                          <span className="rounded-full bg-destructive/10 px-1.5 py-0.5 text-destructive">
                            {t("history.failedBadge")}
                          </span>
                        )}
                        {item.kind === "dictation" && item.data.processing_method.includes("local") && (
                          <span className="rounded-full bg-primary/10 px-1.5 py-0.5 text-primary">
                            {item.data.processing_method.includes("fallback:local")
                              ? t("history.localFallbackBadge")
                              : t("history.localBadge")}
                          </span>
                        )}
                      </div>
                      {noteTitle && (
                        <p className="text-sm font-medium text-foreground-bright mt-0.5 truncate">{noteTitle}</p>
                      )}
                      <p className="text-sm text-foreground mt-0.5 line-clamp-2 whitespace-pre-line">
                        {preview}
                      </p>
                    </div>
                    <ChevronDown
                      className={`w-4 h-4 mt-0.5 shrink-0 text-muted-foreground transition-transform ${isExpanded ? "rotate-180" : ""}`}
                    />
                  </button>

                  {isExpanded && (
                    <div className="px-3 pb-3 space-y-2.5">
                      {failedDictation ? (
                        <div className="rounded-control border border-destructive/25 bg-destructive/5 px-3 py-2">
                          <p className="text-sm font-medium text-destructive">
                            {item.data.error === "empty_transcription"
                              ? t("history.emptyRecording")
                              : t("history.transcriptionFailed")}
                          </p>
                          <p className="mt-1 text-xs text-muted-foreground">
                            {t("history.recordingPreserved")}
                          </p>
                        </div>
                      ) : item.kind === "dictation" && item.data.reconciled_text ? (
                        <div className="space-y-2 rounded-control bg-surface-2 px-3 py-2 max-h-64 overflow-y-auto">
                          <div>
                            <p className="text-[11px] font-medium text-muted-foreground">
                              {t("history.rawAsr")}
                            </p>
                            <p className="text-sm text-foreground whitespace-pre-line">
                              {item.data.original_text}
                            </p>
                          </div>
                          <div>
                            <p className="text-[11px] font-medium text-primary">
                              {t("history.contextualCorrection")}
                            </p>
                            <p className="text-sm text-foreground whitespace-pre-line">
                              {item.data.reconciled_text}
                            </p>
                          </div>
                          {item.data.processed_text
                            && item.data.processed_text !== item.data.reconciled_text && (
                            <div>
                              <p className="text-[11px] font-medium text-muted-foreground">
                                {t("history.finalText")}
                              </p>
                              <p className="text-sm text-foreground whitespace-pre-line">
                                {item.data.processed_text}
                              </p>
                            </div>
                          )}
                        </div>
                      ) : item.kind === "note" && isEditing ? (
                        <div className="space-y-2">
                          <input
                            type="text"
                            value={editDraft.title}
                            onChange={(e) => setEditDraft((d) => ({ ...d, title: e.target.value }))}
                            placeholder={t("notes.titlePlaceholder")}
                            className="w-full h-8 px-2.5 text-sm bg-surface-2 border border-border rounded-control text-foreground placeholder:text-muted-foreground"
                          />
                          <textarea
                            value={editDraft.transcript}
                            onChange={(e) => setEditDraft((d) => ({ ...d, transcript: e.target.value }))}
                            rows={6}
                            className="w-full text-sm text-foreground bg-surface-2 border border-border rounded-control px-3 py-2 resize-y"
                          />
                          <div className="flex items-center justify-end gap-2">
                            <Button variant="ghost" size="sm" onClick={() => setEditingId(null)}>
                              {t("history.confirmCancel")}
                            </Button>
                            <Button variant="outline" size="sm" onClick={() => saveNoteEdit(item)}>
                              {t("notes.save")}
                            </Button>
                          </div>
                        </div>
                      ) : item.kind === "note" && item.data.body_markdown ? (
                        <div className="rounded-control bg-surface-2 px-3 py-2 max-h-64 overflow-y-auto">
                          <MarkdownLite text={stripMarkdownTitle(item.data.body_markdown)} />
                        </div>
                      ) : (
                        <p className="text-sm text-foreground whitespace-pre-line rounded-control bg-surface-2 px-3 py-2 max-h-64 overflow-y-auto">
                          {fullText(item) || t("history.emptyTranscript")}
                        </p>
                      )}

                      <HistoryAudioControls
                        itemKey={item.id}
                        playback={playback}
                        dictation={item.kind === "dictation" ? item.data.audio_asset : undefined}
                        conversation={
                          item.kind === "conversation"
                            ? { me: item.data.audio_asset_me, them: item.data.audio_asset_them }
                            : undefined
                        }
                        noteSegments={item.kind === "note" ? item.data.audio_segments : undefined}
                      />

                      {!isEditing && (
                        <div className="flex items-center justify-end gap-2 flex-wrap">
                          {item.kind === "note" && (
                            <>
                              <Button
                                variant="ghost"
                                size="sm"
                                onClick={() => handleAppendDictation(item)}
                              >
                                <Mic2 className="w-3.5 h-3.5" /> {t("notes.appendDictation")}
                              </Button>
                              <Button
                                variant="ghost"
                                size="sm"
                                onClick={() => handleCleanupNote(item)}
                                disabled={cleaningUpId === item.id}
                              >
                                <Sparkles className="w-3.5 h-3.5" />
                                {cleaningUpId === item.id ? t("notes.cleaningUp") : t("notes.cleanUp")}
                              </Button>
                              <Button variant="ghost" size="sm" onClick={() => startEditingNote(item)}>
                                <Pencil className="w-3.5 h-3.5" /> {t("history.edit")}
                              </Button>
                            </>
                          )}
                          {failedDictation
                            && item.kind === "dictation"
                            && settings.cloudTranscriptionProvider !== "local" && (
                            <Button
                              variant="outline"
                              size="sm"
                              disabled={retryingId === item.id || item.data.audio_asset?.status !== "ready"}
                              onClick={() => void handleRetryTranscription(item)}
                            >
                              <RefreshCw className={`w-3.5 h-3.5 ${retryingId === item.id ? "animate-spin" : ""}`} />
                              {retryingId === item.id ? t("history.retrying") : t("history.retry")}
                            </Button>
                          )}
                          {item.kind === "dictation" && item.data.audio_asset?.status === "ready" && (
                            <Button
                              variant="outline"
                              size="sm"
                              disabled={retryingId === item.id || !settings.localTranscriptionModel}
                              onClick={() => void handleLocalRetranscription(item)}
                            >
                              <RefreshCw className={`w-3.5 h-3.5 ${retryingId === item.id ? "animate-spin" : ""}`} />
                              {t("history.transcribeLocal")}
                            </Button>
                          )}
                          {!failedDictation && (
                            <Button variant="ghost" size="sm" onClick={() => handleCopy(item)}>
                              <Copy className="w-3.5 h-3.5" /> {t("history.copy")}
                            </Button>
                          )}
                          <Button
                            variant="ghost"
                            size="sm"
                            onClick={() => void handleOpenRecordingFolder(item)}
                            disabled={recordingPaths(item).length === 0}
                          >
                            <FolderOpen className="w-3.5 h-3.5" /> {t("history.openFolder")}
                          </Button>
                          {isConfirming ? (
                            <>
                              <span className="text-xs text-muted-foreground">{t("history.confirmDelete")}</span>
                              <Button
                                variant="destructive"
                                size="sm"
                                onClick={() => handleDelete(item)}
                              >
                                {t("history.confirmYes")}
                              </Button>
                              <Button variant="ghost" size="sm" onClick={() => setConfirmDeleteId(null)}>
                                {t("history.confirmCancel")}
                              </Button>
                            </>
                          ) : (
                            <Button
                              variant="destructive"
                              size="sm"
                              onClick={() => setConfirmDeleteId(item.id)}
                            >
                              <Trash2 className="w-3.5 h-3.5" /> {t("history.delete")}
                            </Button>
                          )}
                        </div>
                      )}
                    </div>
                  )}
                </div>
              );
            })}
          </div>
        )}

        {listError && <p className="text-sm text-destructive">{listError}</p>}

        {(hasMoreT || hasMoreC || hasMoreN) && (
          <Button variant="outline" size="sm" onClick={loadMore} disabled={loadingMore}>
            {loadingMore ? t("history.loading") : t("history.loadMore")}
          </Button>
        )}
      </SettingsSection>
    </>
  );
}

function BreakdownRow({ label, periodStats }: { label: string; periodStats: StatsPayload }) {
  const { t } = useTranslation();
  return (
    <li className="flex items-baseline justify-between gap-4">
      <span className="text-foreground">{label}</span>
      <span className="text-muted-foreground tabular-nums">
        {t("stats.recordings", { count: periodStats.total_recordings })} ·{" "}
        {formatDuration(periodStats.total_seconds)}
      </span>
    </li>
  );
}
