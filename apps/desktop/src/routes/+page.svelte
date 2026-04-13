<script lang="ts">
    import { getState, listen, openFile } from '$lib/tauri'
    import ImageViewer from '$lib/components/ImageViewer.svelte'
    import Onboarding from '$lib/components/Onboarding.svelte'

    let isOnboarding: boolean | null = $state(null) // null = loading

    $effect(() => {
        getState().then((state) => {
            isOnboarding = state.onboarding
        })

        // When a file is opened (for example, from Finder) while in onboarding, switch to viewer
        const unlistenPromise = listen<string>('open-file', (event) => {
            openFile(event.payload).then(() => {
                isOnboarding = false
            })
        })

        // Also listen for qa-open-file
        const unlistenQaPromise = listen<string>('qa-open-file', (event) => {
            openFile(event.payload).then(() => {
                isOnboarding = false
            })
        })

        return () => {
            unlistenPromise.then((u) => u())
            unlistenQaPromise.then((u) => u())
        }
    })
</script>

{#if isOnboarding === null}
    <!-- Loading state — matches background so it's invisible -->
{:else if isOnboarding}
    <Onboarding />
{:else}
    <ImageViewer />
{/if}
