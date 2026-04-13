<script lang="ts">
    interface Props {
        filePath: string | null
        index: number
        total: number
        zoom: number
    }

    const { filePath, index, total, zoom }: Props = $props()

    let visible = $state(true)
    let hideTimer: ReturnType<typeof setTimeout> | null = $state(null)

    const filename = $derived(filePath ? (filePath.split('/').pop() ?? '') : '')
    const position = $derived(total > 0 ? `${index + 1} / ${total}` : '')
    const zoomPercent = $derived(`${Math.round(zoom * 100)}%`)

    function scheduleHide() {
        if (hideTimer) clearTimeout(hideTimer)
        visible = true
        hideTimer = setTimeout(() => {
            visible = false
        }, 2000)
    }

    function handleMouseMove() {
        scheduleHide()
    }

    $effect(() => {
        // Show header briefly whenever file changes
        if (filePath) scheduleHide()
    })

    $effect(() => {
        window.addEventListener('mousemove', handleMouseMove)
        return () => window.removeEventListener('mousemove', handleMouseMove)
    })
</script>

{#if filePath}
    <div class="header" class:header--visible={visible}>
        <div class="header__info">
            <span class="header__filename">{filename}</span>
            {#if position}
                <span class="header__position">{position}</span>
            {/if}
            <span class="header__zoom">{zoomPercent}</span>
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
        /* Inset from edges for titlebar traffic lights */
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
