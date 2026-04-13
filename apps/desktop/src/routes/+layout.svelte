<script lang="ts">
    import '../app.css'
    import { onMount, type Snippet } from 'svelte'
    import { initLogBridge } from '$lib/log-bridge'

    interface Props {
        children: Snippet
    }

    const { children }: Props = $props()

    // Initialize log bridge in onMount (not top-level) to avoid interfering with
    // Svelte 5's reactivity system. Early logs before mount are lost but reactivity works.
    onMount(() => {
        initLogBridge()
    })
</script>

{@render children()}
