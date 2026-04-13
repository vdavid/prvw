<script lang="ts">
    import { onMount, onDestroy } from 'svelte'

    interface Props {
        filePath: string | null
        index: number
        total: number
        zoom: number
    }

    const { filePath, index, total, zoom }: Props = $props()

    // DOM refs for imperative updates (Svelte reactivity doesn't re-render in Tauri WKWebView).
    // Intentionally NOT $state — we update these via bind:this and read them imperatively.
    let headerEl = $state<HTMLDivElement | undefined>(undefined)
    let filenameEl = $state<HTMLSpanElement | undefined>(undefined)
    let positionEl = $state<HTMLSpanElement | undefined>(undefined)
    let zoomEl = $state<HTMLSpanElement | undefined>(undefined)

    let hideTimer: ReturnType<typeof setTimeout> | null = null

    function scheduleHide() {
        if (hideTimer) clearTimeout(hideTimer)
        headerEl?.classList.add('header--visible')
        hideTimer = setTimeout(() => {
            headerEl?.classList.remove('header--visible')
        }, 2000)
    }

    /** Imperative update called by ImageViewer after every state change.
     * We manipulate the DOM directly because Svelte's reactive template updates
     * don't fire in Tauri's WKWebView (see CLAUDE.md gotcha). */

    export function update(fp: string | null, idx: number, tot: number, z: number) {
        if (filenameEl) filenameEl.textContent = fp ? (fp.split('/').pop() ?? '') : ''
        if (positionEl) positionEl.textContent = tot > 0 ? `${idx + 1} / ${tot}` : ''
        if (zoomEl) zoomEl.textContent = `${Math.round(z * 100)}%`
        scheduleHide()
    }

    function handleMouseMove() {
        scheduleHide()
    }

    onMount(() => {
        // Initial render
        update(filePath, index, total, zoom)
        window.addEventListener('mousemove', handleMouseMove)
    })

    onDestroy(() => {
        window.removeEventListener('mousemove', handleMouseMove)
        if (hideTimer) clearTimeout(hideTimer)
    })
</script>

{#if filePath}
    <div class="header header--visible" bind:this={headerEl}>
        <div class="header__info">
            <span class="header__filename" bind:this={filenameEl}></span>
            <span class="header__position" bind:this={positionEl}></span>
            <span class="header__zoom" bind:this={zoomEl}></span>
        </div>
    </div>
{/if}

<style>
    .header {
        position: fixed;
        top: 0;
        left: 0;
        right: 0;
        z-index: 10;
        padding: var(--spacing-lg) var(--spacing-lg) var(--spacing-xl);
        background: linear-gradient(to bottom, var(--color-header-gradient-start), var(--color-header-gradient-end));
        opacity: 0;
        transition: opacity 0.3s ease;
        pointer-events: none;
        padding-top: var(--spacing-titlebar);
    }

    .header--visible {
        opacity: 1;
    }

    .header__info {
        display: flex;
        align-items: baseline;
        gap: var(--spacing-sm);
        font-family: var(--font-sans);
        font-size: var(--font-size-sm);
    }

    .header__filename {
        color: var(--color-text-bright);
        font-weight: 500;
    }

    .header__position {
        color: var(--color-text-muted);
    }

    .header__zoom {
        color: var(--color-text-muted);
        margin-left: auto;
    }
</style>
