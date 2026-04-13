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

    let containerEl: HTMLDivElement | undefined = $state(undefined)
    let naturalWidth = $state(0)
    let naturalHeight = $state(0)

    const MIN_ZOOM = 1
    const MAX_ZOOM = 100
    const ZOOM_STEP = 1.15

    // ---------------------------------------------------------------------------
    // Image carousel — pool of pre-loaded <img> elements for instant switching.
    // The current image is visible; adjacent images (N-2..N+2) are hidden but
    // already fetched and decoded by the browser. Navigation swaps visibility.
    // ---------------------------------------------------------------------------

    /** Maps file path → pre-created <img> element */
    const imagePool = new Map<string, HTMLImageElement>()

    /** The currently visible <img> element (the one with the transform applied) */
    let activeImg: HTMLImageElement | null = null

    // Track which images are fully decoded (ready for instant display)
    const decodedPaths = new Set<string>()

    function basename(fp: string): string {
        return fp.split('/').pop() ?? fp
    }

    // ---------------------------------------------------------------------------
    // Image loading pipeline with 5 timed log points:
    //   [1] src set       — browser starts fetching via asset protocol
    //   [2] onload        — bytes received from IPC
    //   [3] decoded       — img.decode() resolved, bitmap in GPU memory
    //   [4] shown         — opacity/display set to visible
    //   [5] painted       — double-rAF, pixels actually on screen
    // ---------------------------------------------------------------------------

    /** Create a pool img element. Starts hidden (display:none).
     * Does NOT set src — call loadImage() to start fetching. */
    function createPoolImg(filePath: string): HTMLImageElement {
        const img = document.createElement('img')
        img.className = 'viewer__image'
        img.draggable = false
        img.alt = ''
        img.style.display = 'none'
        img.style.pointerEvents = 'none'
        imagePool.set(filePath, img)
        if (containerEl) containerEl.appendChild(img)
        return img
    }

    /** Start fetching + decoding an image. Logs [1] src set, [2] onload, [3] decoded.
     * No-op if already loading or decoded. */
    function loadImage(filePath: string) {
        const img = imagePool.get(filePath)
        if (!img || img.src || decodedPaths.has(filePath)) return

        const t0 = performance.now()
        const name = basename(filePath)

        console.log(`[viewer] [1] src set - ${name}`)
        img.src = convertFileSrc(filePath)

        img.onload = () => {
            const t1 = performance.now()
            console.log(`[viewer] [2] onload - ${name} (${(t1 - t0).toFixed(0)}ms)`)

            // img.decode() resolves when the bitmap is ready in memory
            img.decode()
                .then(() => {
                    const t2 = performance.now()
                    decodedPaths.add(filePath)
                    console.log(`[viewer] [3] decoded - ${name} (${(t2 - t1).toFixed(0)}ms)`)

                    // If this is the active image, show it now
                    if (img === activeImg) {
                        displayActiveImage()
                    }
                })
                .catch(() => {
                    // decode() can fail if src was cleared (aborted). Ignore.
                })
        }
    }

    /** Switch the visible image. If already decoded, swap is instant (no fetch needed). */
    function showImage(filePath: string) {
        // Cancel in-flight fetches that aren't for this image or already decoded
        abortNonEssentialFetches(filePath)

        // Get or create the img element
        let img = imagePool.get(filePath)
        if (!img) {
            img = createPoolImg(filePath)
        }

        // Hide previous
        if (activeImg && activeImg !== img) {
            activeImg.style.display = 'none'
            activeImg.style.transform = ''
        }

        activeImg = img

        // Already decoded? Show instantly.
        if (decodedPaths.has(filePath) && img.naturalWidth > 0) {
            displayActiveImage()
        } else {
            // Start loading (if not already). onload → decode → displayActiveImage
            loadImage(filePath)
        }
    }

    /** Make the active image visible and fit to window. Logs [4] shown, [5] painted. */
    function displayActiveImage() {
        if (!activeImg || !containerEl) return
        const name = currentFilePath ? basename(currentFilePath) : '?'
        const t3 = performance.now()

        // [4] shown — make visible
        activeImg.style.display = ''
        naturalWidth = activeImg.naturalWidth
        naturalHeight = activeImg.naturalHeight
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
        console.log(`[viewer] [4] shown - ${name} (${(performance.now() - t3).toFixed(0)}ms)`)

        // [5] painted — double rAF: first rAF = browser scheduled paint, second = paint committed
        requestAnimationFrame(() => {
            requestAnimationFrame(() => {
                console.log(`[viewer] [5] painted - ${name} (${(performance.now() - t3).toFixed(0)}ms)`)
                // NOW it's safe to preload adjacent images — current is on screen
                preloadAdjacent()
            })
        })
    }

    /** Cancel in-flight fetches for non-essential images. */
    function abortNonEssentialFetches(keepPath: string) {
        for (const [path, img] of imagePool) {
            if (path !== keepPath && !decodedPaths.has(path) && img.src) {
                img.onload = null
                img.src = ''
            }
        }
    }

    /** Pre-load adjacent images. Called ONLY after the current image is painted. */
    function preloadAdjacent() {
        getAdjacentPaths(2)
            .then((paths) => {
                for (const p of paths) {
                    if (!imagePool.has(p)) {
                        createPoolImg(p)
                    }
                    loadImage(p)
                }
                prunePool(paths)
                console.log(
                    `[viewer] preload: ${paths.length} paths, pool=${imagePool.size}, decoded=${decodedPaths.size}`,
                )
            })
            .catch(() => {})
    }

    /** Remove pool entries outside the adjacent window. Aborts fetches first. */
    function prunePool(adjacentPaths: string[]) {
        const keep = new Set(adjacentPaths)
        if (currentFilePath) keep.add(currentFilePath)
        for (const [path, img] of imagePool) {
            if (!keep.has(path)) {
                img.onload = null
                img.src = ''
                img.remove()
                imagePool.delete(path)
                decodedPaths.delete(path)
            }
        }
    }

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
    // ---------------------------------------------------------------------------

    function applyTransform() {
        if (!activeImg) return
        activeImg.style.transform = `translate(${panX}px, ${panY}px) scale(${zoom})`
        activeImg.style.transformOrigin = '0 0'
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

    let navStartTime = 0

    function applyNavResponse(resp: NavigateResponse) {
        if (resp.filePath) {
            currentFilePath = resp.filePath
            currentIndex = resp.index
            currentTotal = resp.total
            const wasDecoded = decodedPaths.has(resp.filePath)
            showImage(resp.filePath)
            const totalMs = navStartTime > 0 ? performance.now() - navStartTime : 0
            console.log(
                `[viewer] swap: ${basename(resp.filePath)} total=${totalMs.toFixed(0)}ms preloaded=${wasDecoded} pool=${imagePool.size}`,
            )
            // preloadAdjacent() is called from onActiveImageLoad after the image displays
        }
    }

    async function doNavigate(forward: boolean) {
        try {
            navStartTime = performance.now()
            const resp = await tauriNavigate(forward)
            const ipcMs = performance.now() - navStartTime
            console.log(`[viewer] nav IPC: ${ipcMs.toFixed(0)}ms`)
            applyNavResponse(resp)
        } catch (e) {
            console.error('Navigation failed:', e)
        }
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
    // Lifecycle: onMount + onDestroy
    // ---------------------------------------------------------------------------

    const unlisteners: Array<() => void> = []

    onMount(() => {
        getState().then((s) => {
            if (!s.onboarding && s.filePath) {
                currentFilePath = s.filePath
                currentIndex = s.index
                currentTotal = s.total
                showImage(s.filePath)
                // preloadAdjacent() is called from onActiveImageLoad after the image displays
            }
        })

        // Tauri event listeners
        listen<string>('open-file', (event) => {
            openFile(event.payload).then((resp) => {
                applyNavResponse(resp)
            })
        }).then((u) => unlisteners.push(u))

        listen<string>('menu-action', (event) => {
            handleMenuAction(event.payload)
        }).then((u) => unlisteners.push(u))

        listen<NavigateResponse>('state-changed', (event) => {
            applyNavResponse(event.payload)
        }).then((u) => unlisteners.push(u))

        listen<boolean>('qa-navigate', (event) => {
            doNavigate(event.payload)
        }).then((u) => unlisteners.push(u))

        listen<string>('qa-open-file', (event) => {
            openFile(event.payload).then((resp) => {
                applyNavResponse(resp)
            })
        }).then((u) => unlisteners.push(u))

        listen('qa-toggle-fullscreen', () => toggleFullscreen()).then((u) => unlisteners.push(u))

        listen<boolean>('qa-set-fullscreen', (event) => {
            setFullscreen(event.payload)
        }).then((u) => unlisteners.push(u))

        listen('qa-fit-to-window', () => fitToWindow()).then((u) => unlisteners.push(u))
        listen('qa-actual-size', () => actualSize()).then((u) => unlisteners.push(u))
        listen<number>('qa-set-zoom', (event) => zoomTo(event.payload)).then((u) => unlisteners.push(u))
        listen<string>('qa-send-key', (event) => {
            onKeyDown(new KeyboardEvent('keydown', { key: event.payload }))
        }).then((u) => unlisteners.push(u))

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
        // Clean up pool
        for (const [, img] of imagePool) img.remove()
        imagePool.clear()
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
    <!-- Carousel images are appended to this div imperatively via getOrCreateImg() -->
</div>

<Header bind:this={headerEl} filePath={currentFilePath} index={currentIndex} total={currentTotal} {zoom} />

<style>
    .viewer {
        position: fixed;
        inset: 0;
        overflow: hidden;
        background: var(--color-bg);
    }

    /* Applied to carousel <img> elements created in JS */
    :global(.viewer__image) {
        position: absolute;
        top: 0;
        left: 0;
        will-change: transform;
        image-rendering: auto;
        pointer-events: none;
    }
</style>
