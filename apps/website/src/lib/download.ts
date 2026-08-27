import latestRelease from '../../public/latest.json'

export const version = latestRelease.version

/** Where someone lands when there's no build for their platform. */
export const sourceUrl = 'https://github.com/vdavid/prvw'

const platforms = latestRelease.platforms

export const dmgUrls = {
  aarch64: platforms['darwin-aarch64'].url,
  x86_64: platforms['darwin-x86_64'].url,
  universal: platforms['darwin-universal'].url,
}

function formatBytes(bytes: number): string {
  return `${Math.round(bytes / (1024 * 1024))} MB`
}

const rawSizes = (
  latestRelease as {
    dmgSizes?: { aarch64: number; x86_64: number; universal: number }
  }
).dmgSizes

/** Formatted download sizes (for example, "23 MB"), null if not yet populated by CI */
export const dmgSizes =
  rawSizes && rawSizes.universal > 0
    ? {
        aarch64: formatBytes(rawSizes.aarch64),
        x86_64: formatBytes(rawSizes.x86_64),
        universal: formatBytes(rawSizes.universal),
      }
    : null

/**
 * The Windows installer (`PrvwSetup-<version>-x64.exe`). Releases that predate the Windows leg carry
 * no `windows-x86_64` key, so every read here tolerates the key being missing and the UI drops the
 * Windows option rather than linking at a 404. The cast is what lets the key be absent: TypeScript
 * types `platforms` off whatever `latest.json` happens to hold at build time.
 */
const windowsPlatform = (platforms as Record<string, { url: string; size?: number } | undefined>)['windows-x86_64']

export const windowsUrl = windowsPlatform?.url ?? null

/**
 * Byte size of the installer. CI may record it on the platform entry or in a top-level
 * `installerSizes` map next to `dmgSizes`; either spelling works, and neither is required.
 */
const rawWindowsSize =
  windowsPlatform?.size ?? (latestRelease as { installerSizes?: { x86_64?: number } }).installerSizes?.x86_64

/** Formatted installer size (for example, "12 MB"), null when there's no size to show */
export const windowsSize = rawWindowsSize && rawWindowsSize > 0 ? formatBytes(rawWindowsSize) : null
