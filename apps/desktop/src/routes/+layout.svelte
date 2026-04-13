<script lang="ts">
    import '../app.css'
    import type { Snippet } from 'svelte'
    import { initLogBridge } from '$lib/log-bridge'

    // Initialize log bridge immediately (before any component mounts)
    // so that console.log calls in $effect callbacks are captured.
    initLogBridge()

    interface Props {
        children: Snippet
    }

    const { children }: Props = $props()
</script>

{@render children()}
