<script lang="ts">
	import { Badge, CopyButton, IconButton, Text } from '@dorsk/tsumikit';
	import { type PageText, pageSrcset, pageUrl } from '#lib/api';

	let {
		id,
		page,
		total,
		version,
		showText,
		busy,
		onrotate,
		ondelete,
		ontext,
	}: {
		id: string;
		page: PageText;
		total: number;
		version: string;
		showText: boolean;
		busy: boolean;
		onrotate: (degrees: 90 | 270) => void;
		ondelete: () => void;
		ontext: () => void;
	} = $props();

	const SOURCE = { layer: 'text layer', ocr: 'OCR', extracted: 'extracted', none: 'no text' };
	const n = $derived(page.number);
	const hasText = $derived(page.text.trim() !== '');
	let loaded = $state(false);
</script>

<article class="sheet" id="page-{n}" aria-label="Page {n} of {total}">
	<header>
		<Text size="sm" tone="muted" numeric>Page {n} of {total}</Text>
		<Badge size="xs" tone="muted">{SOURCE[page.source]}</Badge>
		<span class="tools">
			<IconButton
				icon="file-text"
				label={showText ? `Hide the text of page ${n}` : `Show the text of page ${n}`}
				variant="ghost"
				box="sm"
				pressed={showText}
				disabled={!hasText}
				onclick={ontext}
			/>
			<IconButton
				label="Rotate page {n} left"
				variant="ghost"
				box="sm"
				disabled={busy}
				onclick={() => onrotate(270)}
			>
				<path d="M3 12a9 9 0 1 0 9-9 9.75 9.75 0 0 0-6.74 2.74L3 8" /><path d="M3 3v5h5" />
			</IconButton>
			<IconButton
				label="Rotate page {n} right"
				variant="ghost"
				box="sm"
				disabled={busy}
				onclick={() => onrotate(90)}
			>
				<path d="M21 12a9 9 0 1 1-9-9c2.52 0 4.93 1 6.74 2.74L21 8" /><path d="M21 3v5h-5" />
			</IconButton>
			<IconButton
				icon="trash"
				label="Delete page {n}"
				variant="ghost"
				box="sm"
				hoverDanger
				disabled={busy || total < 2}
				onclick={ondelete}
			/>
		</span>
	</header>
	{#key version}
		<div class="paper" class:loaded>
			<img
				src={pageUrl(id, n, 1200, version)}
				srcset={pageSrcset(id, n, [600, 1200, 2000], version)}
				sizes="(min-width: 64rem) min(52rem, 60vw), 100vw"
				alt="Page {n}"
				loading="lazy"
				decoding="async"
				onload={() => (loaded = true)}
			/>
		</div>
	{/key}
	{#if showText && hasText}
		<div class="text">
			<div class="copy"><CopyButton text={page.text} label="Copy the text of page {n}" box="sm" /></div>
			<Text as="p" size="sm" class="ocr" lang="">{page.text}</Text>
		</div>
	{/if}
</article>

<style>
	.sheet {
		display: flex;
		flex-direction: column;
		gap: var(--sp-2);
		scroll-margin-top: calc(var(--shell-header-h, var(--header-h)) + var(--sp-4));
	}
	header {
		display: flex;
		align-items: center;
		gap: var(--sp-2);
	}
	.tools {
		display: inline-flex;
		gap: var(--sp-1);
		margin-inline-start: auto;
	}
	.paper {
		aspect-ratio: 210 / 297;
		background: var(--bg-elevated);
		border: 1px solid var(--border);
		border-radius: var(--radius-sm);
		overflow: hidden;
	}
	.paper.loaded {
		aspect-ratio: auto;
	}
	.paper img {
		display: block;
		inline-size: 100%;
		block-size: auto;
	}
	.text {
		position: relative;
		background: var(--bg-elevated);
		border-radius: var(--radius-md);
		padding: var(--sp-3) var(--sp-10) var(--sp-3) var(--sp-3);
	}
	.text :global(.ocr) {
		white-space: pre-wrap;
		overflow-wrap: anywhere;
		margin: 0;
		user-select: text;
	}
	.copy {
		position: absolute;
		inset-block-start: var(--sp-2);
		inset-inline-end: var(--sp-2);
	}
</style>
