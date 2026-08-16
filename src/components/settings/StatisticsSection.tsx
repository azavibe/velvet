import { useEffect, useMemo, useState } from "react";
import { useTranslation } from "react-i18next";
import { Copy, Trash2, ChevronDown, Mic, MessagesSquare } from "lucide-react";
import {
  getStats,
  getTranscriptions,
  deleteTranscription,
  listConversations,
  getConversation,
  deleteConversation,
  setClipboardText,
  type StatsPayload,
  type Transcription,
  type ConversationSummary,
} from "@/services/tauriApi";
import { SettingsSection } from "@/components/ui/SettingsSection";
import { Button } from "@/components/ui/button";

type Loaded = { today: StatsPayload; week: StatsPayload; all: StatsPayload };

type ToastFn = (props: { title?: string; description?: string; variant: "default" | "destructive" | "success" }) => void;

// One page of each source per "Load more" click — small enough to keep the
// merged list's memory footprint well under the ~10MB budget even with a
// few pages loaded, since only summaries (not full conversation transcripts)
// are fetched up front.
const PAGE_SIZE = 20;

type HistoryItem =
  | { kind: "dictation"; id: string; timestamp: string; data: Transcription }
  | { kind: "conversation"; id: string; timestamp: string; data: ConversationSummary };

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

export default function StatisticsSection({ toast }: { toast?: ToastFn }) {
  const { t } = useTranslation();
  const [stats, setStats] = useState<Loaded | null>(null);
  const [statsError, setStatsError] = useState<string | null>(null);

  const [transcriptions, setTranscriptions] = useState<Transcription[]>([]);
  const [conversations, setConversations] = useState<ConversationSummary[]>([]);
  const [tOffset, setTOffset] = useState(0);
  const [cOffset, setCOffset] = useState(0);
  const [hasMoreT, setHasMoreT] = useState(true);
  const [hasMoreC, setHasMoreC] = useState(true);
  const [loadingMore, setLoadingMore] = useState(false);
  const [listError, setListError] = useState<string | null>(null);

  const [expandedId, setExpandedId] = useState<string | null>(null);
  const [expandedDetail, setExpandedDetail] = useState<Record<string, string>>({});
  const [confirmDeleteId, setConfirmDeleteId] = useState<string | null>(null);

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

  const loadMore = async () => {
    setLoadingMore(true);
    setListError(null);
    try {
      const [nextT, nextC] = await Promise.all([
        hasMoreT ? getTranscriptions(PAGE_SIZE, tOffset) : Promise.resolve([]),
        hasMoreC ? listConversations(PAGE_SIZE, cOffset) : Promise.resolve([]),
      ]);
      if (nextT.length > 0) {
        setTranscriptions((prev) => [...prev, ...nextT]);
        setTOffset((o) => o + nextT.length);
      }
      if (nextT.length < PAGE_SIZE) setHasMoreT(false);
      if (nextC.length > 0) {
        setConversations((prev) => [...prev, ...nextC]);
        setCOffset((o) => o + nextC.length);
      }
      if (nextC.length < PAGE_SIZE) setHasMoreC(false);
    } catch (e) {
      setListError(e instanceof Error ? e.message : String(e));
    } finally {
      setLoadingMore(false);
    }
  };

  useEffect(() => {
    loadMore();
    // Only on mount — subsequent pages come from the "Load more" button.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);

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
    ];
    merged.sort((a, b) => Date.parse(b.timestamp) - Date.parse(a.timestamp));
    return merged;
  }, [transcriptions, conversations]);

  const toggleExpand = async (item: HistoryItem) => {
    if (expandedId === item.id) {
      setExpandedId(null);
      return;
    }
    setExpandedId(item.id);
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
      if (item.kind === "dictation") {
        await deleteTranscription(item.data.id);
        setTranscriptions((prev) => prev.filter((tr) => tr.id !== item.data.id));
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
              const duration =
                item.kind === "dictation"
                  ? item.data.duration_ms != null
                    ? formatDuration(item.data.duration_ms / 1000)
                    : null
                  : (() => {
                      const s = conversationDurationSeconds(item.data);
                      return s != null ? formatDuration(s) : null;
                    })();
              const preview =
                item.kind === "dictation"
                  ? snippetLines(item.data.processed_text || item.data.original_text)
                  : snippetLines(item.data.snippet || t("history.emptyTranscript"));

              return (
                <div key={item.id} className="rounded-control bg-surface-1 overflow-hidden">
                  <button
                    type="button"
                    onClick={() => toggleExpand(item)}
                    className="w-full text-left px-3 py-2.5 flex items-start gap-3 hover:bg-surface-2 transition-colors"
                  >
                    {item.kind === "dictation" ? (
                      <Mic className="w-4 h-4 mt-0.5 shrink-0 text-muted-foreground" />
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
                            : t("history.typeConversation")}
                        </span>
                      </div>
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
                      <p className="text-sm text-foreground whitespace-pre-line rounded-control bg-surface-2 px-3 py-2 max-h-64 overflow-y-auto">
                        {fullText(item) || t("history.emptyTranscript")}
                      </p>
                      <div className="flex items-center justify-end gap-2">
                        <Button variant="ghost" size="sm" onClick={() => handleCopy(item)}>
                          <Copy className="w-3.5 h-3.5" /> {t("history.copy")}
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
                    </div>
                  )}
                </div>
              );
            })}
          </div>
        )}

        {listError && <p className="text-sm text-destructive">{listError}</p>}

        {(hasMoreT || hasMoreC) && (
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
