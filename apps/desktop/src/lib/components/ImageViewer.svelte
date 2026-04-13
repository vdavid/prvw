<script lang="ts">
    import { onMount, onDestroy } from 'svelte'
    import Header from './Header.svelte'
    import {
        getState,
        navigate as tauriNavigate,
        getAdjacentPaths,
        toggleFullscreen,
        setFullscreen,
        handleEscape,
        reportZoomPan,
        openFile,
        convertFileSrc,
        listen,
        type NavigateResponse,
    } from '$lib/tauri'

    // ---------------------------------------------------------------------------
    // State (all in-component, matching Cmdr's pattern)
    // ---------------------------------------------------------------------------

    let currentFilePath: string | null = $state(null)
    let currentIndex = $state(0)
    let currentTotal = $state(0)

    let zoom = $state(1)
    let panX = $state(0)
    let panY = $state(0)

    let isDragging = $state(false)
    let dragStartX = 0
    let dragStartY = 0
    let panStartX = 0
    let panStartY = 0

    let imageEl: HTMLImageElement | undefined = $state(undefined)
    let containerEl: HTMLDivElement | undefined = $state(undefined)

    let naturalWidth = $state(0)
    let naturalHeight = $state(0)

    const MIN_ZOOM = 1
    const MAX_ZOOM = 100
    const ZOOM_STEP = 1.15

    // ---------------------------------------------------------------------------
    // Derived
    // ---------------------------------------------------------------------------

    const imageSrc = $derived(currentFilePath ? convertFileSrc(currentFilePath) : '')

    const fitZoom = $derived.by(() => {
        if (!naturalWidth || !naturalHeight || !containerEl) return 1
        const cw = containerEl.clientWidth
        const ch = containerEl.clientHeight
        if (cw === 0 || ch === 0) return 1
        return Math.min(cw / naturalWidth, ch / naturalHeight)
    })

    const cursorStyle = $derived.by(() => {
        if (isDragging) return 'grabbing'
        if (zoom > fitZoom) return 'grab'
        return 'default'
    })

    // ---------------------------------------------------------------------------
    // Transform — applied imperatively after every mutation.
    // Svelte 5's $effect/template reactivity doesn't re-fire in this Tauri
    // WKWebView (signals update but effects and DOM bindings don't re-render).
    // We work around this by calling applyTransform() and updateHeader()
    // explicitly after every state change.
    // ---------------------------------------------------------------------------

    function applyTransform() {
        if (!imageEl) return
        imageEl.style.transform = `translate(${panX}px, ${panY}px) scale(${zoom})`
        imageEl.style.transformOrigin = '0 0'
    }

    let headerEl: { update: (f: string | null, i: number, t: number, z: number) => void } | undefined

    function updateUI() {
        applyTransform()
        headerEl?.update(currentFilePath, currentIndex, currentTotal, zoom)
        scheduleReport()
    }

    // ---------------------------------------------------------------------------
    // Report zoom/pan to Rust (debounced)
    // ---------------------------------------------------------------------------

    let reportTimer: ReturnType<typeof setTimeout> | null = null

    function scheduleReport() {
        if (reportTimer) clearTimeout(reportTimer)
        reportTimer = setTimeout(() => {
            if (!containerEl) return
            reportZoomPan(zoom, panX, panY, containerEl.clientWidth, containerEl.clientHeight)
        }, 100)
    }

    // ---------------------------------------------------------------------------
    // Pan clamping
    // ---------------------------------------------------------------------------

    function clampPan(z: number, px: number, py: number): [number, number] {
        if (!containerEl || !naturalWidth || !naturalHeight) return [px, py]
        const cw = containerEl.clientWidth
        const ch = containerEl.clientHeight
        const scaledW = naturalWidth * z
        const scaledH = naturalHeight * z

        const cx = scaledW <= cw ? (cw - scaledW) / 2 : Math.min(0, Math.max(cw - scaledW, px))
        const cy = scaledH <= ch ? (ch - scaledH) / 2 : Math.min(0, Math.max(ch - scaledH, py))
        return [cx, cy]
    }

    // ---------------------------------------------------------------------------
    // Zoom helpers
    // ---------------------------------------------------------------------------

    function zoomTo(newZoom: number, anchorX?: number, anchorY?: number) {
        if (!containerEl) return
        const clamped = Math.min(MAX_ZOOM, Math.max(MIN_ZOOM * fitZoom, newZoom))
        const prevZoom = zoom
        const ax = anchorX ?? containerEl.clientWidth / 2
        const ay = anchorY ?? containerEl.clientHeight / 2
        const newPanX = ax - (ax - panX) * (clamped / prevZoom)
        const newPanY = ay - (ay - panY) * (clamped / prevZoom)
        zoom = clamped
        const [cx, cy] = clampPan(zoom, newPanX, newPanY)
        panX = cx
        panY = cy
        updateUI()
    }

    function fitToWindow() {
        zoom = fitZoom
        const [cx, cy] = clampPan(zoom, 0, 0)
        panX = cx
        panY = cy
        updateUI()
    }

    function actualSize() {
        zoomTo(1)
    }

    // ---------------------------------------------------------------------------
    // Navigation
    // ---------------------------------------------------------------------------

    function applyNavResponse(resp: NavigateResponse) {
        if (resp.filePath) {
            currentFilePath = resp.filePath
            currentIndex = resp.index
            currentTotal = resp.total
            // Imperatively update img src (template reactivity won't re-render)
            if (imageEl) {
                imageEl.src = convertFileSrc(resp.filePath)
            }
            updateUI()
        }
    }

    async function doNavigate(forward: boolean) {
        try {
            const resp = await tauriNavigate(forward)
            applyNavResponse(resp)
            preloadAdjacent()
        } catch (e) {
            console.error('Navigation failed:', e)
        }
    }

    function preloadAdjacent() {
        getAdjacentPaths(3)
            .then((paths) => {
                for (const p of paths) {
                    const img = new Image()
                    img.src = convertFileSrc(p)
                }
            })
            .catch(() => {})
    }

    // ---------------------------------------------------------------------------
    // Event handlers
    // ---------------------------------------------------------------------------

    function onWheel(e: WheelEvent) {
        e.preventDefault()
        const factor = e.deltaY < 0 ? ZOOM_STEP : 1 / ZOOM_STEP
        zoomTo(zoom * factor, e.clientX, e.clientY)
    }

    function onMouseDown(e: MouseEvent) {
        if (e.button !== 0 || zoom <= fitZoom) return
        isDragging = true
        dragStartX = e.clientX
        dragStartY = e.clientY
        panStartX = panX
        panStartY = panY
    }

    function onMouseMove(e: MouseEvent) {
        if (!isDragging) return
        const [cx, cy] = clampPan(zoom, panStartX + (e.clientX - dragStartX), panStartY + (e.clientY - dragStartY))
        panX = cx
        panY = cy
        applyTransform()
    }

    function onMouseUp() {
        isDragging = false
    }

    function onDoubleClick() {
        if (zoom > fitZoom) {
            fitToWindow()
        } else {
            actualSize()
        }
    }

    function onImageLoad() {
        if (!imageEl || !containerEl) return
        naturalWidth = imageEl.naturalWidth
        naturalHeight = imageEl.naturalHeight
        const cw = containerEl.clientWidth
        const ch = containerEl.clientHeight
        if (cw > 0 && ch > 0 && naturalWidth > 0 && naturalHeight > 0) {
            const fz = Math.min(cw / naturalWidth, ch / naturalHeight)
            zoom = fz
            const [cx, cy] = clampPan(fz, 0, 0)
            panX = cx
            panY = cy
            updateUI()
        }
    }

    function onKeyDown(e: KeyboardEvent) {
        switch (e.key) {
            case 'ArrowRight':
            case ' ':
                e.preventDefault()
                doNavigate(true)
                break
            case 'ArrowLeft':
            case 'Backspace':
                e.preventDefault()
                doNavigate(false)
                break
            case 'f':
            case 'Enter':
                e.preventDefault()
                toggleFullscreen()
                break
            case 'Escape':
                e.preventDefault()
                handleEscape()
                break
            case '0':
                e.preventDefault()
                fitToWindow()
                break
            case '1':
                e.preventDefault()
                actualSize()
                break
            case '=':
            case '+':
                e.preventDefault()
                zoomTo(zoom * ZOOM_STEP)
                break
            case '-':
                e.preventDefault()
                zoomTo(zoom / ZOOM_STEP)
                break
        }
    }

    function handleMenuAction(action: string) {
        switch (action) {
            case 'zoom_in':
                zoomTo(zoom * ZOOM_STEP)
                break
            case 'zoom_out':
                zoomTo(zoom / ZOOM_STEP)
                break
            case 'actual_size':
                actualSize()
                break
            case 'fit_to_window':
                fitToWindow()
                break
            case 'fullscreen':
                toggleFullscreen()
                break
            case 'next':
                doNavigate(true)
                break
            case 'previous':
                doNavigate(false)
                break
        }
    }

    // ---------------------------------------------------------------------------
    // Lifecycle: onMount + onDestroy (matching Cmdr's pattern, NOT $effect)
    // ---------------------------------------------------------------------------

    const unlisteners: Array<() => void> = []

    onMount(() => {
        // Load initial state
        getState().then((s) => {
            if (!s.onboarding && s.filePath) {
                currentFilePath = s.filePath
                currentIndex = s.index
                currentTotal = s.total
                preloadAdjacent()
            }
        })

        // Tauri event listeners
        listen<string>('open-file', (event) => {
            openFile(event.payload).then((resp) => {
                applyNavResponse(resp)
                preloadAdjacent()
            })
        }).then((u) => unlisteners.push(u))

        listen<string>('menu-action', (event) => {
            handleMenuAction(event.payload)
        }).then((u) => unlisteners.push(u))

        listen<NavigateResponse>('state-changed', (event) => {
            applyNavResponse(event.payload)
            preloadAdjacent()
        }).then((u) => unlisteners.push(u))

        listen<boolean>('qa-navigate', (event) => {
            doNavigate(event.payload)
        }).then((u) => unlisteners.push(u))

        listen<string>('qa-open-file', (event) => {
            openFile(event.payload).then((resp) => {
                applyNavResponse(resp)
                preloadAdjacent()
            })
        }).then((u) => unlisteners.push(u))

        listen('qa-toggle-fullscreen', () => {
            toggleFullscreen()
        }).then((u) => unlisteners.push(u))

        listen<boolean>('qa-set-fullscreen', (event) => {
            setFullscreen(event.payload)
        }).then((u) => unlisteners.push(u))

        listen('qa-fit-to-window', () => fitToWindow()).then((u) => unlisteners.push(u))
        listen('qa-actual-size', () => actualSize()).then((u) => unlisteners.push(u))
        listen<number>('qa-set-zoom', (event) => zoomTo(event.payload)).then((u) => unlisteners.push(u))
        listen<string>('qa-send-key', (event) => {
            onKeyDown(new KeyboardEvent('keydown', { key: event.payload }))
        }).then((u) => unlisteners.push(u))

        // Window events
        window.addEventListener('keydown', onKeyDown)
        window.addEventListener('resize', handleResize)
        window.addEventListener('mouseup', onMouseUp)
    })

    function handleResize() {
        const [cx, cy] = clampPan(zoom, panX, panY)
        panX = cx
        panY = cy
        updateUI()
    }

    onDestroy(() => {
        for (const u of unlisteners) u()
        window.removeEventListener('keydown', onKeyDown)
        window.removeEventListener('resize', handleResize)
        window.removeEventListener('mouseup', onMouseUp)
        if (reportTimer) clearTimeout(reportTimer)
    })
</script>

<!-- eslint-disable-next-line svelte/no-unused-svelte-ignore -- needed for svelte-check a11y warning below -->
<!-- svelte-ignore a11y_no_noninteractive_element_interactions -->
<div
    class="viewer"
    bind:this={containerEl}
    style:cursor={cursorStyle}
    onwheel={onWheel}
    onmousedown={onMouseDown}
    onmousemove={onMouseMove}
    ondblclick={onDoubleClick}
    role="application"
    aria-label="Image viewer"
>
    {#if imageSrc}
        <img bind:this={imageEl} src={imageSrc} alt="" class="viewer__image" onload={onImageLoad} draggable="false" />
    {/if}
</div>

<Header bind:this={headerEl} filePath={currentFilePath} index={currentIndex} total={currentTotal} {zoom} />

<style>
    .viewer {
        position: fixed;
        inset: 0;
        overflow: hidden;
        background: var(--color-bg);
    }

    .viewer__image {
        position: absolute;
        top: 0;
        left: 0;
        will-change: transform;
        image-rendering: auto;
        pointer-events: none;
    }
</style>
