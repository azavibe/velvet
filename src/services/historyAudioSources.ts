import type { AudioPlaybackSource } from "@/services/audioPlayback";

export interface HistoryAudioAssetRef {
  id: number;
}

export interface HistoryAudioSourceResult {
  url: string;
  duration_ms: number | null;
}

/** Resolve opaque asset URLs without changing order or loading media bytes.
 * Play All may skip an unavailable segment; a single-track request remains
 * strict so its control can show the failure. */
export async function resolveHistoryAudioSources(
  assets: HistoryAudioAssetRef[],
  resolve: (assetId: number) => Promise<HistoryAudioSourceResult>,
  skipUnavailable: boolean,
): Promise<AudioPlaybackSource[]> {
  const results = await Promise.all(
    assets.map(async (asset): Promise<AudioPlaybackSource | null> => {
      try {
        const source = await resolve(asset.id);
        return {
          key: `asset-${asset.id}`,
          url: source.url,
          durationMs: source.duration_ms,
        };
      } catch (error) {
        if (skipUnavailable) return null;
        throw error;
      }
    }),
  );
  return results.filter((source): source is AudioPlaybackSource => source !== null);
}
