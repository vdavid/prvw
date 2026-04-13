<script lang="ts">
    import { getVersion } from '@tauri-apps/api/app'
    import { open } from '@tauri-apps/plugin-shell'
    import { getCurrentWindow } from '@tauri-apps/api/window'

    let version = $state('...')

    $effect(() => {
        getVersion().then((v) => {
            version = v
        })

        function onKeyDown(e: KeyboardEvent) {
            if (e.key === 'Escape') {
                e.preventDefault()
                getCurrentWindow().close()
            }
        }

        window.addEventListener('keydown', onKeyDown)
        return () => window.removeEventListener('keydown', onKeyDown)
    })

    function openWebsite(e: MouseEvent) {
        e.preventDefault()
        open('https://getprvw.com')
    }
</script>

<div class="about">
    <img class="about__icon" src="/icon.png" alt="Prvw" width="64" height="64" />
    <h1 class="about__name">Prvw {version}</h1>
    <p class="about__tagline">A fast image viewer for macOS.</p>
    <p class="about__author">By David Veszelovszki</p>
    <div class="about__links">
        <a class="about__link" href="https://getprvw.com" onclick={openWebsite}>getprvw.com</a>
    </div>
    <p class="about__copyright">&copy; 2025&ndash;2026 Rymdskottkarra AB</p>
</div>

<style>
    .about {
        display: flex;
        flex-direction: column;
        align-items: center;
        justify-content: center;
        min-height: 100vh;
        padding: var(--spacing-xl);
        background: transparent;
        font-family: var(--font-sans);
        text-align: center;
        -webkit-app-region: drag;
        gap: var(--spacing-xs);
    }

    .about__icon {
        width: 64px;
        height: 64px;
        border-radius: var(--radius-md);
        margin-bottom: var(--spacing-md);
    }

    .about__name {
        font-size: var(--font-size-xl);
        font-weight: 600;
        color: var(--color-text-bright);
        margin: 0;
    }

    .about__tagline {
        font-size: var(--font-size-sm);
        color: var(--color-text);
        margin: var(--spacing-xs) 0 0;
    }

    .about__author {
        font-size: var(--font-size-sm);
        color: var(--color-text-muted);
        margin: 0;
    }

    .about__links {
        margin-top: var(--spacing-md);
        -webkit-app-region: no-drag;
    }

    .about__link {
        font-size: var(--font-size-sm);
        color: var(--color-accent);
        text-decoration: none;
    }

    .about__link:hover {
        text-decoration: underline;
    }

    .about__copyright {
        margin-top: var(--spacing-md);
        font-size: var(--font-size-xs);
        color: var(--color-text-muted);
    }
</style>
