<script lang="ts">
	import { Button, EmptyState } from '@dorsk/tsumikit';
	import { resolve } from '$app/paths';
	import { page } from '$app/state';

	const notFound = $derived(page.status === 404);
	const title = $derived(
		notFound ? 'There is nothing at this address' : 'Something went wrong on our side',
	);
	const description = $derived(
		notFound
			? 'The link may be old, or the document may have been moved. Your library is not affected.'
			: 'The page could not be loaded. Nothing you did caused this; trying again usually works.',
	);
</script>

<svelte:head>
	<title>{title} · violet</title>
</svelte:head>

<EmptyState tone="danger" icon="alert-circle" {title} {description}>
	{#snippet action()}
		{#if notFound}
			<Button variant="primary" href={resolve('/')}>Go to search</Button>
		{:else}
			<Button variant="primary" onclick={() => location.reload()}>Try again</Button>
			<Button href={resolve('/')}>Go to search</Button>
		{/if}
	{/snippet}
</EmptyState>
