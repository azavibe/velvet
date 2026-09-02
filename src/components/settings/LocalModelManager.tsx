import { useCallback, useEffect, useRef, useState } from "react";
import { Check, Download, Loader2, Square, Trash2 } from "lucide-react";
import { Button } from "@/components/ui/button";
import { useTranslation } from "react-i18next";
import {
  cancelLocalModelDownload,
  deleteLocalModel,
  downloadLocalModel,
  listLocalModels,
  onLocalModelDownloadProgress,
  type LocalModelDownloadProgress,
  type LocalModelInfo,
} from "@/services/tauriApi";

interface LocalModelManagerProps {
  selectedModel: string;
  onModelChange: (model: string) => void;
}

function formatBytes(bytes: number): string {
  if (bytes >= 1024 ** 3) return `${(bytes / 1024 ** 3).toFixed(1)} GB`;
  return `${Math.round(bytes / 1024 ** 2)} MB`;
}

export default function LocalModelManager({
  selectedModel,
  onModelChange,
}: LocalModelManagerProps) {
  const { t } = useTranslation();
  const [models, setModels] = useState<LocalModelInfo[]>([]);
  const [progress, setProgress] = useState<Record<string, LocalModelDownloadProgress>>({});
  const [error, setError] = useState("");
  const selectedModelRef = useRef(selectedModel);
  const onModelChangeRef = useRef(onModelChange);
  selectedModelRef.current = selectedModel;
  onModelChangeRef.current = onModelChange;

  const refresh = useCallback(async () => {
    const next = await listLocalModels();
    setModels(next);
    if (!next.some((model) => model.id === selectedModelRef.current && model.installed)) {
      const replacement = next.find((model) => model.installed)?.id ?? "";
      if (replacement !== selectedModelRef.current) onModelChangeRef.current(replacement);
    }
  }, []);

  useEffect(() => {
    void refresh().catch((cause) => setError(String(cause)));
  }, [refresh]);

  useEffect(() => {
    let unlisten: (() => void) | undefined;
    void onLocalModelDownloadProgress((event) => {
      setProgress((current) => ({ ...current, [event.model_id]: event }));
      if (event.status === "installed") void refresh();
    }).then((cleanup) => {
      unlisten = cleanup;
    });
    return () => unlisten?.();
  }, [refresh]);

  async function startDownload(modelId: string) {
    setError("");
    setModels((current) =>
      current.map((model) => (model.id === modelId ? { ...model, downloading: true } : model)),
    );
    try {
      await downloadLocalModel(modelId);
      onModelChange(modelId);
    } catch (cause) {
      if (!String(cause).includes("cancelled")) setError(String(cause));
    } finally {
      await refresh();
    }
  }

  async function remove(modelId: string) {
    setError("");
    try {
      await deleteLocalModel(modelId);
      if (selectedModel === modelId) onModelChange("");
      await refresh();
    } catch (cause) {
      setError(String(cause));
    }
  }

  return (
    <div className="w-full space-y-2">
      {models.map((model) => {
        const current = progress[model.id];
        const downloaded = current?.downloaded_bytes ?? model.downloaded_bytes;
        const percent = Math.min(100, Math.round((downloaded / model.size_bytes) * 100));
        return (
          <div
            key={model.id}
            className={`rounded-control border p-3 transition-colors ${
              selectedModel === model.id ? "border-primary bg-primary/5" : "border-border bg-surface-1"
            }`}
          >
            <div className="flex items-center gap-3">
              <button
                type="button"
                disabled={!model.installed}
                onClick={() => onModelChange(model.id)}
                className="min-w-0 flex-1 text-left disabled:cursor-default"
              >
                <span className="flex items-center gap-2 text-sm font-medium">
                  {model.name}
                  {selectedModel === model.id && <Check className="h-4 w-4 text-primary" />}
                </span>
                <span className="text-xs text-muted-foreground">
                  {formatBytes(model.size_bytes)} · {model.quantization}
                </span>
              </button>
              {model.downloading ? (
                <Button
                  type="button"
                  size="sm"
                  variant="outline"
                  aria-label={t("localModels.cancel")}
                  onClick={() => void cancelLocalModelDownload(model.id)}
                >
                  <Square className="mr-1 h-3.5 w-3.5" /> {percent}%
                </Button>
              ) : model.installed ? (
                <Button
                  type="button"
                  size="icon"
                  variant="ghost"
                  aria-label={t("localModels.delete")}
                  onClick={() => void remove(model.id)}
                >
                  <Trash2 className="h-4 w-4" />
                </Button>
              ) : (
                <Button type="button" size="sm" variant="outline" onClick={() => void startDownload(model.id)}>
                  <Download className="mr-1 h-4 w-4" /> {t("localModels.download")}
                </Button>
              )}
            </div>
            {model.downloading && (
              <div className="mt-2 h-1.5 overflow-hidden rounded-full bg-border">
                <div className="h-full bg-primary transition-[width]" style={{ width: `${percent}%` }} />
              </div>
            )}
          </div>
        );
      })}
      {!models.length && !error && (
        <p className="flex items-center gap-2 text-xs text-muted-foreground">
          <Loader2 className="h-3.5 w-3.5 animate-spin" /> {t("localModels.loading")}
        </p>
      )}
      {error && <p className="text-xs text-destructive">{error}</p>}
      <p className="text-xs text-muted-foreground">
        {t("localModels.privacy")}
      </p>
    </div>
  );
}
