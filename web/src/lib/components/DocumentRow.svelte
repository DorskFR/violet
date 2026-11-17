<script lang="ts">
	import { Artwork, Badge, Text } from '@dorsk/tsumikit';
	import { resolve } from '$app/paths';
	import { type Hit, pageSrcset, pageUrl } from '#lib/api';
	import { formatAmount, formatDate, pageCount } from '#lib/format';
	import Snippet from './Snippet.svelte';

	let { hit }: { hit: Hit } = $props();

	const amount = $derived(formatAmount(hit.amount_minor, hit.currency));
</script>

<a class="row" href={resolve('/documents/[id]', { id: hit.id })} data-result>
	<Artwork
		src={pageUrl(hit.id, 1, 120)}
		srcset={pageSrcset(hit.id, 1, [120, 240])}
		sizes="3.25rem"
		alt=""
		seed={hit.title}
		aspect="210/297"
		size="3.25rem"
		radius="sm"
		fallback="icon"
		icon="file-text"
	/>
	<div class="main">
		<div class="head">
			<Text weight="medium" class="title">{hit.title}</Text>
			{#if amount}
				<Text numeric nowrap weight="medium">{amount}</Text>
			{/if}
		</div>
		<div class="meta">
			{#if hit.issuer}<span>{hit.issuer}</span>{/if}
			{#if hit.date}<time datetime={hit.date}>{formatDate(hit.date)}</time>{/if}
			<span>{pageCount(hit.pages)}</span>
		</div>
		{#if hit.snippet}
			<p class="snippet"><Snippet html={hit.snippet} /></p>
		{/if}
		<div class="tags">
			<Badge size="xs" tone="violet">{hit.area}</Badge>
			<Badge size="xs" tone="muted">{hit.doc_type}</Badge>
			{#each hit.tags.slice(0, 4) as tag (tag)}
				<Badge size="xs" variant="text" tone="muted">#{tag}</Badge>
			{/each}
		</div>
	</div>
</a>

<style>
	.row {
		display: grid;
		grid-template-columns: auto minmax(0, 1fr);
		gap: var(--sp-3);
		align-items: start;
		padding: var(--sp-3);
		border-radius: var(--radius-md);
		color: var(--text);
		text-decoration: none;
	}
	.row:hover {
		background: var(--bg-elevated);
	}
	.row:focus-visible {
		outline: 2px solid var(--accent);
		outline-offset: -2px;
	}
	.main {
		display: flex;
		flex-direction: column;
		gap: var(--sp-1);
		min-inline-size: 0;
	}
	.head {
		display: flex;
		align-items: baseline;
		justify-content: space-between;
		gap: var(--sp-3);
	}
	.head :global(.title) {
		overflow-wrap: anywhere;
	}
	.meta {
		display: flex;
		flex-wrap: wrap;
		column-gap: var(--sp-2);
		color: var(--text-muted);
		font-size: var(--fs-sm);
	}
	.meta > * + *::before {
		content: '·';
		margin-inline-end: var(--sp-2);
		color: var(--text-faint);
	}
	.snippet {
		margin: 0;
		color: var(--text-muted);
		font-size: var(--fs-sm);
		display: -webkit-box;
		-webkit-line-clamp: 2;
		line-clamp: 2;
		-webkit-box-orient: vertical;
		overflow: hidden;
	}
	.tags {
		display: flex;
		flex-wrap: wrap;
		gap: var(--sp-1);
		align-items: center;
	}
</style>
