<script lang="ts">
	import { parseSnippet } from '#lib/snippet';

	let { html }: { html: string | null | undefined } = $props();

	const parts = $derived(parseSnippet(html));
</script>

{#each parts as part, i (i)}{#if part.mark}<mark>{part.text}</mark>{:else}{part.text}{/if}{/each}
