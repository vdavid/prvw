<script lang="ts">
    import { load } from '@tauri-apps/plugin-store'

    let checkForUpdates = $state(true)
    let storeReady = $state(false)
    let storeError: string | null = $state(null)

    // We hold a reference to the store so we can save on toggle
    let store: Awaited<ReturnType<typeof load>> | null = $state(null)

    $effect(() => {
        load('settings.json', { defaults: { checkForUpdates: true }, autoSave: true })
            .then(async (s) => {
                store = s
                const val = await s.get<boolean>('checkForUpdates')
                if (val !== null && val !== undefined) {
                    checkForUpdates = val
                }
                storeReady = true
            })
            .catch((e) => {
                console.error('Failed to load settings store:', e)
                storeError = String(e)
                // Still show the UI with defaults so the window isn't stuck on "Loading..."
                storeReady = true
            })
    })

    async function onToggle() {
        checkForUpdates = !checkForUpdates
        if (store) {
            await store.set('checkForUpdates', checkForUpdates)
        }
    }
</script>

<div class="settings">
    <h1 class="settings__title">Settings</h1>

    {#if storeReady}
        {#if storeError}
            <p class="settings__error">Couldn't load settings: {storeError}</p>
        {/if}
        <div class="settings__card">
            <div class="settings__row">
                <div class="settings__label-group">
                    <span class="settings__label">Check for updates</span>
                    <span class="settings__description">Periodically check for new versions of Prvw</span>
                </div>
                <button
                    class="toggle"
                    class:toggle--on={checkForUpdates}
                    onclick={onToggle}
                    role="switch"
                    aria-checked={checkForUpdates}
                    aria-label="Check for updates"
                >
                    <span class="toggle__knob"></span>
                </button>
            </div>
        </div>
    {:else}
        <p class="settings__loading">Loading...</p>
    {/if}
</div>

<style>
    .settings {
        min-height: 100vh;
        background: transparent;
        padding: var(--spacing-2xl) var(--spacing-xl);
        font-family: var(--font-sans);
        /* Allow dragging window from settings */
        -webkit-app-region: drag;
    }

    .settings__title {
        font-size: var(--font-size-lg);
        font-weight: 600;
        color: var(--color-text-bright);
        margin: 0 0 var(--spacing-lg) 0;
        text-align: center;
    }

    .settings__card {
        background: var(--color-surface);
        border: 1px solid var(--color-border);
        border-radius: var(--radius-md);
        padding: var(--spacing-md);
        -webkit-app-region: no-drag;
    }

    .settings__row {
        display: flex;
        align-items: center;
        justify-content: space-between;
        gap: var(--spacing-md);
    }

    .settings__label-group {
        display: flex;
        flex-direction: column;
        gap: var(--spacing-xs);
    }

    .settings__label {
        font-size: var(--font-size-md);
        color: var(--color-text);
        font-weight: 500;
    }

    .settings__description {
        font-size: var(--font-size-sm);
        color: var(--color-text-muted);
    }

    .settings__loading {
        font-size: var(--font-size-md);
        color: var(--color-text-muted);
        text-align: center;
    }

    .settings__error {
        font-size: var(--font-size-sm);
        color: var(--color-error);
        text-align: center;
        margin: 0 0 var(--spacing-md) 0;
    }

    /* Toggle switch */
    .toggle {
        --spacing-toggle-width: 44px;
        --spacing-toggle-height: 24px;
        --spacing-toggle-knob: 18px;

        position: relative;
        width: var(--spacing-toggle-width);
        height: var(--spacing-toggle-height);
        background: var(--color-toggle-off);
        border: none;
        border-radius: var(--radius-full);
        cursor: pointer;
        padding: 0;
        flex-shrink: 0;
        transition: background 0.2s ease;
    }

    .toggle--on {
        background: var(--color-accent);
    }

    .toggle__knob {
        position: absolute;
        top: 3px;
        left: 3px;
        width: var(--spacing-toggle-knob);
        height: var(--spacing-toggle-knob);
        background: var(--color-text-bright);
        border-radius: 50%;
        transition: transform 0.2s ease;
    }

    .toggle--on .toggle__knob {
        transform: translateX(20px);
    }
</style>
