import latestRelease from "../../public/latest.json";

export const version = latestRelease.version;

const platforms = latestRelease.platforms;

export const dmgUrls = {
  aarch64: platforms["darwin-aarch64"].url,
  x86_64: platforms["darwin-x86_64"].url,
  universal: platforms["darwin-universal"].url,
};

function formatBytes(bytes: number): string {
  return `${Math.round(bytes / (1024 * 1024))} MB`;
}

const rawSizes = (latestRelease as { dmgSizes?: { aarch64: number; x86_64: number; universal: number } }).dmgSizes;

/** Formatted download sizes (for example, "23 MB"), null if not yet populated by CI */
export const dmgSizes =
  rawSizes && rawSizes.universal > 0
    ? {
        aarch64: formatBytes(rawSizes.aarch64),
        x86_64: formatBytes(rawSizes.x86_64),
        universal: formatBytes(rawSizes.universal),
      }
    : null;
