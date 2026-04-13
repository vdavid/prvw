<script lang="ts">
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
    // State
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

    // Natural image dimensions (set on load)
    let naturalWidth = $state(0)
    let naturalHeight = $state(0)

    const MIN_ZOOM = 1
    const MAX_ZOOM = 100
    const ZOOM_STEP = 1.15

    // ---------------------------------------------------------------------------
    // Derived
    // ---------------------------------------------------------------------------

    const imageSrc = $derived(currentFilePath ? convertFileSrc(currentFilePath) : '')

    // Fit zoom: the zoom level at which the image fills the viewport without cropping.
    // We compute this whenever the image or window dimensions change.
    const fitZoom = $derived.by(() => {
        if (!naturalWidth || !naturalHeight || !containerEl) return 1
        const cw = containerEl.clientWidth
        const ch = containerEl.clientHeight
        if (cw === 0 || ch === 0) return 1
        return Math.min(cw / naturalWidth, ch / naturalHeight)
    })

    // The cursor style depends on zoom and drag state
    const cursorStyle = $derived.by(() => {
        if (isDragging) return 'grabbing'
        if (zoom > fitZoom) return 'grab'
        return 'default'
    })

    // Apply transform imperatively. Svelte 5's $effect/$derived reactivity doesn't reliably
    // re-fire in this component (observed: $state mutations in event handlers don't trigger
    // effect re-runs). So we call applyTransform() explicitly after every state change.
    function applyTransform() {
        if (!imageEl) return
        imageEl.style.transform = `translate(${panX}px, ${panY}px) scale(${zoom})`
        imageEl.style.transformOrigin = '0 0'
    }

    // ---------------------------------------------------------------------------
    // Report zoom/pan to Rust (fire and forget, debounced)
    // ---------------------------------------------------------------------------

    let reportTimer: ReturnType<typeof setTimeout> | null = null

    function scheduleReport() {
        if (reportTimer) clearTimeout(reportTimer)
        reportTimer = setTimeout(() => {
            if (!containerEl) return
            reportZoomPan(zoom, panX, panY, containerEl.clientWidth, containerEl.clientHeight)
        }, 100)
    }

    $effect(() => {
        void zoom
        void panX
        void panY
        scheduleReport()
    })

    // ---------------------------------------------------------------------------
    // Pan clamping
    // ---------------------------------------------------------------------------

    function clampPan(z: number, px: number, py: number): [number, number] {
        if (!containerEl || !naturalWidth || !naturalHeight) return [px, py]

        const cw = containerEl.clientWidth
        const ch = containerEl.clientHeight
        const scaledW = naturalWidth * z
        const scaledH = naturalHeight * z

        let cx: number
        let cy: number

        if (scaledW <= cw) {
            // Image fits horizontally — center it
            cx = (cw - scaledW) / 2
        } else {
            // Image overflows — clamp so edges don't pull past viewport
            cx = Math.min(0, Math.max(cw - scaledW, px))
        }

        if (scaledH <= ch) {
            cy = (ch - scaledH) / 2
        } else {
            cy = Math.min(0, Math.max(ch - scaledH, py))
        }

        return [cx, cy]
    }

    // ---------------------------------------------------------------------------
    // Zoom helpers
    // ---------------------------------------------------------------------------

    function zoomTo(newZoom: number, anchorX?: number, anchorY?: number) {
        if (!containerEl) {
            console.warn('[viewer] zoomTo: no containerEl!')
            return
        }

        const clamped = Math.min(MAX_ZOOM, Math.max(MIN_ZOOM * fitZoom, newZoom))
        const prevZoom = zoom

        // Anchor defaults to viewport center
        const ax = anchorX ?? containerEl.clientWidth / 2
        const ay = anchorY ?? containerEl.clientHeight / 2

        // Adjust pan so the point under the anchor stays fixed
        const newPanX = ax - (ax - panX) * (clamped / prevZoom)
        const newPanY = ay - (ay - panY) * (clamped / prevZoom)

        zoom = clamped
        const [cx, cy] = clampPan(zoom, newPanX, newPanY)
        panX = cx
        panY = cy
        applyTransform()
    }

    function fitToWindow() {
        zoom = fitZoom
        const [cx, cy] = clampPan(zoom, 0, 0)
        panX = cx
        panY = cy
        applyTransform()
    }

    function actualSize() {
        // 1:1 pixel mapping
        zoomTo(1)
    }

    // ---------------------------------------------------------------------------
    // Navigation
    // ---------------------------------------------------------------------------

    function applyNavResponse(resp: NavigateResponse) {
        console.log('[viewer] applyNavResponse:', resp)
        if (resp.filePath) {
            currentFilePath = resp.filePath
            currentIndex = resp.index
            currentTotal = resp.total
            // Imperatively update the img src (Svelte reactivity doesn't re-render in this component)
            if (imageEl) {
                imageEl.src = convertFileSrc(resp.filePath)
                console.log('[viewer] set img src to:', resp.filePath)
            }
        }
    }

    async function doNavigate(forward: boolean) {
        console.log('[viewer] doNavigate forward=%o', forward)
        try {
            const resp = await tauriNavigate(forward)
            applyNavResponse(resp)
            preloadAdjacent()
        } catch (e) {
            console.error('Navigation failed:', e)
        }
    }

    async function preloadAdjacent() {
        try {
            const paths = await getAdjacentPaths(3)
            for (const p of paths) {
                const img = new Image()
                img.src = convertFileSrc(p)
            }
        } catch {
            // Preloading is best-effort
        }
    }

    // ---------------------------------------------------------------------------
    // Event handlers
    // ---------------------------------------------------------------------------

    function onWheel(e: WheelEvent) {
        e.preventDefault()
        const factor = e.deltaY < 0 ? ZOOM_STEP : 1 / ZOOM_STEP
        console.log('[viewer] onWheel deltaY=%d factor=%f zoom=%f->%f', e.deltaY, factor, zoom, zoom * factor)
        zoomTo(zoom * factor, e.clientX, e.clientY)
    }

    function onMouseDown(e: MouseEvent) {
        if (e.button !== 0) return
        if (zoom <= fitZoom) {
            console.log('[viewer] onMouseDown: zoom(%f) <= fitZoom(%f), ignoring', zoom, fitZoom)
            return
        }
        console.log('[viewer] onMouseDown: starting drag')
        isDragging = true
        dragStartX = e.clientX
        dragStartY = e.clientY
        panStartX = panX
        panStartY = panY
    }

    function onMouseMove(e: MouseEvent) {
        if (!isDragging) return
        const dx = e.clientX - dragStartX
        const dy = e.clientY - dragStartY
        const [cx, cy] = clampPan(zoom, panStartX + dx, panStartY + dy)
        panX = cx
        panY = cy
        applyTransform()
    }

    function onMouseUp() {
        isDragging = false
    }

    function onDoubleClick() {
        console.log('[viewer] onDoubleClick zoom=%f fitZoom=%f', zoom, fitZoom)
        if (zoom > fitZoom) {
            fitToWindow()
        } else {
            actualSize()
        }
    }

    function onImageLoad() {
        console.log('[viewer] onImageLoad imageEl=%o containerEl=%o', !!imageEl, !!containerEl)
        if (!imageEl || !containerEl) {
            console.warn('[viewer] onImageLoad: bailing, imageEl=%o containerEl=%o', imageEl, containerEl)
            return
        }
        naturalWidth = imageEl.naturalWidth
        naturalHeight = imageEl.naturalHeight
        const cw = containerEl.clientWidth
        const ch = containerEl.clientHeight
        console.log(
            '[viewer] onImageLoad: natural=%dx%d container=%dx%d',
            naturalWidth,
            naturalHeight,
            cw,
            ch,
        )
        if (cw > 0 && ch > 0 && naturalWidth > 0 && naturalHeight > 0) {
            const fz = Math.min(cw / naturalWidth, ch / naturalHeight)
            zoom = fz
            const [cx, cy] = clampPan(fz, 0, 0)
            panX = cx
            panY = cy
            applyTransform()
            console.log('[viewer] onImageLoad: set zoom=%f pan=[%f,%f]', fz, cx, cy)
        } else {
            console.warn('[viewer] onImageLoad: zero dimensions, not setting zoom')
        }
    }

    function onKeyDown(e: KeyboardEvent) {
        // Prevent defaults for keys we handle
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

    // ---------------------------------------------------------------------------
    // Window resize: recalculate pan bounds
    // ---------------------------------------------------------------------------

    function onResize() {
        const [cx, cy] = clampPan(zoom, panX, panY)
        panX = cx
        panY = cy
        applyTransform()
    }

    // ---------------------------------------------------------------------------
    // Initialization and Tauri event listeners
    // ---------------------------------------------------------------------------

    $effect(() => {
        console.log('[viewer] $effect: mounting, loading initial state')
        getState().then((state) => {
            console.log('[viewer] getState result:', state)
            if (!state.onboarding && state.filePath) {
                currentFilePath = state.filePath
                currentIndex = state.index
                currentTotal = state.total
                preloadAdjacent()
            }
        })

        // Tauri event listeners
        const unlisteners: Array<() => void> = []

        listen<string>('open-file', (event) => {
            openFile(event.payload).then((resp) => {
                applyNavResponse(resp)
                preloadAdjacent()
            })
        }).then((u) => unlisteners.push(u))

        listen<string>('menu-action', (event) => {
            console.log('[viewer] menu-action event:', event.payload)
            handleMenuAction(event.payload)
        }).then((u) => unlisteners.push(u))

        listen<NavigateResponse>('state-changed', (event) => {
            console.log('[viewer] state-changed event:', event.payload)
            applyNavResponse(event.payload)
            preloadAdjacent()
        }).then((u) => unlisteners.push(u))

        // QA events
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

        listen('qa-fit-to-window', () => {
            fitToWindow()
        }).then((u) => unlisteners.push(u))

        listen('qa-actual-size', () => {
            actualSize()
        }).then((u) => unlisteners.push(u))

        listen<number>('qa-set-zoom', (event) => {
            zoomTo(event.payload)
        }).then((u) => unlisteners.push(u))

        listen<string>('qa-send-key', (event) => {
            const key = event.payload
            onKeyDown(new KeyboardEvent('keydown', { key }))
        }).then((u) => unlisteners.push(u))

        // Keyboard and resize
        window.addEventListener('keydown', onKeyDown)
        window.addEventListener('resize', onResize)
        window.addEventListener('mouseup', onMouseUp)

        return () => {
            for (const u of unlisteners) u()
            window.removeEventListener('keydown', onKeyDown)
            window.removeEventListener('resize', onResize)
            window.removeEventListener('mouseup', onMouseUp)
        }
    })

    // ---------------------------------------------------------------------------
    // Menu action handler
    // ---------------------------------------------------------------------------

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
        <img
            bind:this={imageEl}
            src={imageSrc}
            alt=""
            class="viewer__image"
            onload={onImageLoad}
            draggable="false"
        />
    {/if}
</div>

<Header filePath={currentFilePath} index={currentIndex} total={currentTotal} {zoom} />

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
        /* Prevent image from being selectable/draggable */
        pointer-events: none;
    }
</style>
