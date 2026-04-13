<script lang="ts">
    import { getOnboardingInfo, setAsDefaultViewer, handleEscape, type OnboardingInfo } from '$lib/tauri'

    let info: OnboardingInfo | null = $state(null)
    let statusMessage: string | null = $state(null)
    let statusIsError = $state(false)
    let isSettingDefault = $state(false)

    $effect(() => {
        getOnboardingInfo().then((i) => {
            info = i
        })

        function onKeyDown(e: KeyboardEvent) {
            if (e.key === 'Enter') {
                e.preventDefault()
                doSetDefault()
            } else if (e.key === 'Escape') {
                e.preventDefault()
                handleEscape()
            }
        }

        window.addEventListener('keydown', onKeyDown)
        return () => window.removeEventListener('keydown', onKeyDown)
    })

    async function doSetDefault() {
        if (isSettingDefault) return
        isSettingDefault = true
        statusMessage = null
        statusIsError = false
        try {
            const msg = await setAsDefaultViewer()
            statusMessage = msg
            statusIsError = false
            // Refresh info to show updated handler status
            info = await getOnboardingInfo()
        } catch (e) {
            statusMessage = `Failed: ${e}`
            statusIsError = true
        } finally {
            isSettingDefault = false
        }
    }
</script>

<div class="onboarding">
    {#if info}
        <div class="onboarding__content">
            <h1 class="onboarding__title">Prvw v{info.version}</h1>
            <p class="onboarding__subtitle">A fast, minimal image viewer</p>

            <div class="onboarding__instructions">
                <p>Open an image file to get started, or set Prvw as your default viewer:</p>

                <button class="onboarding__button" onclick={doSetDefault} disabled={isSettingDefault}>
                    {#if isSettingDefault}
                        <span class="spinner"></span> Setting...
                    {:else}
                        Set as default image viewer
                    {/if}
                </button>

                {#if statusMessage}
                    <p
                        class="onboarding__status"
                        class:onboarding__status--success={!statusIsError}
                        class:onboarding__status--error={statusIsError}
                    >
                        {statusMessage}
                    </p>
                {/if}

                {#if info.handlerStatus}
                    <div class="onboarding__handlers">
                        {#each info.handlerStatus.trim().split('\n') as line (line)}
                            <p class="onboarding__handler-line">{line.trim()}</p>
                        {/each}
                    </div>
                {/if}

                {#if info.notInApplications}
                    <p class="onboarding__tip">Tip: Move Prvw to your /Applications folder for the best experience.</p>
                {/if}
            </div>

            <div class="onboarding__footer">
                <span class="onboarding__hint">Press Enter to set as default, Esc to close</span>
            </div>
        </div>
    {:else}
        <div class="onboarding__loading">Loading...</div>
    {/if}
</div>

<style>
    .onboarding {
        display: flex;
        align-items: center;
        justify-content: center;
        height: 100vh;
        background: transparent;
        padding: var(--spacing-2xl);
    }

    .onboarding__content {
        text-align: center;
        max-width: 420px;
    }

    .onboarding__title {
        font-family: var(--font-sans);
        font-size: var(--font-size-2xl);
        font-weight: 600;
        color: var(--color-text-bright);
        margin: 0 0 var(--spacing-xs) 0;
    }

    .onboarding__subtitle {
        font-family: var(--font-sans);
        font-size: var(--font-size-lg);
        color: var(--color-text-muted);
        margin: 0 0 var(--spacing-2xl) 0;
    }

    .onboarding__instructions {
        font-family: var(--font-sans);
        font-size: var(--font-size-md);
        color: var(--color-text);
        line-height: 1.6;
    }

    .onboarding__instructions p {
        margin: 0 0 var(--spacing-md) 0;
    }

    .onboarding__button {
        display: inline-block;
        padding: var(--spacing-sm) var(--spacing-lg);
        font-family: var(--font-sans);
        font-size: var(--font-size-md);
        font-weight: 500;
        color: var(--color-text-bright);
        background: var(--color-accent);
        border: none;
        border-radius: var(--radius-sm);
        cursor: pointer;
        transition: opacity 0.15s ease;
    }

    .onboarding__button:hover:not(:disabled) {
        opacity: 0.85;
    }

    .onboarding__button:active:not(:disabled) {
        opacity: 0.7;
    }

    .onboarding__button:disabled {
        opacity: 0.6;
        cursor: default;
    }

    .spinner {
        display: inline-block;
        width: 14px;
        height: 14px;
        border: 2px solid var(--color-spinner-track);
        border-top-color: var(--color-text-bright);
        border-radius: 50%;
        animation: spin 0.6s linear infinite;
        vertical-align: middle;
        margin-right: var(--spacing-xs);
    }

    @keyframes spin {
        to {
            transform: rotate(360deg);
        }
    }

    .onboarding__status {
        font-family: var(--font-sans);
        font-size: var(--font-size-sm);
    }

    .onboarding__status--success {
        color: var(--color-success);
    }

    .onboarding__status--error {
        color: var(--color-error);
    }

    .onboarding__handlers {
        margin-top: var(--spacing-md);
        text-align: center;
    }

    .onboarding__handler-line {
        font-family: var(--font-sans);
        font-size: var(--font-size-sm);
        color: var(--color-text-muted);
        margin: var(--spacing-xs) 0;
    }

    .onboarding__tip {
        font-family: var(--font-sans);
        font-size: var(--font-size-sm);
        color: var(--color-text-muted);
        font-style: italic;
    }

    .onboarding__footer {
        margin-top: var(--spacing-xl);
    }

    .onboarding__hint {
        font-family: var(--font-sans);
        font-size: var(--font-size-sm);
        color: var(--color-text-muted);
    }

    .onboarding__loading {
        font-family: var(--font-sans);
        font-size: var(--font-size-md);
        color: var(--color-text-muted);
    }
</style>
