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
 * The Windows installer (`PrvwSetup-<version>-x64.exe`), from the optional `windows-x86_64` platform
 * entry. The cast is what lets the key be absent: TypeScript types `platforms` off whatever
 * `latest.json` happens to hold at build time, and today that's the three DMGs and nothing else.
 *
 * A URL alone doesn't count as released. `latest.json` is a **build-time static import**, so
 * whatever sits in the file is baked into the deployed site: a placeholder entry becomes a live
 * button pointing at a 404. So an entry only counts when it also carries a real byte size, which CI
 * can only know once it has an installer in hand. Same convention as `dmgSizes` above.
 */
const windowsPlatform = (platforms as Record<string, { url: string; size?: number } | undefined>)['windows-x86_64']

const windowsReleased = Boolean(windowsPlatform?.url) && (windowsPlatform?.size ?? 0) > 0

/** Installer URL, null until a release actually ships one */
export const windowsUrl = windowsReleased ? (windowsPlatform?.url ?? null) : null

/** Formatted installer size (for example, "12 MB"), null whenever `windowsUrl` is */
export const windowsSize = windowsReleased ? formatBytes(windowsPlatform?.size ?? 0) : null
