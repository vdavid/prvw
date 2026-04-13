import latestRelease from '../../public/latest.json'

export const version = latestRelease.version

const platforms = latestRelease.platforms

export const dmgUrls = {
    aarch64: platforms['darwin-aarch64'].url,
    x86_64: platforms['darwin-x86_64'].url,
    universal: platforms['darwin-universal'].url,
}
