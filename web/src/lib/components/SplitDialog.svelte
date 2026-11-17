<script lang="ts">
	import { Button, IconButton, Modal, Text } from '@dorsk/tsumikit';
	import { pageUrl } from '#lib/api';
	import { describeGroup, splitGroups, toggleCut } from '#lib/split';

	let {
		open = $bindable(false),
		id,
		pages,
		version,
		onsplit,
	}: {
		open: boolean;
		id: string;
		pages: number;
		version: string;
		onsplit: (groups: number[][]) => Promise<void>;
	} = $props();

	let cuts = $state<number[]>([]);
	let busy = $state(false);
	let failure = $state('');

	const groups = $derived(splitGroups(pages, cuts));
	const groupOf = $derived(
		new Map(groups.flatMap((group, i) => group.map((page) => [page, i] as const))),
	);

	$effect(() => {
		if (open) {
			cuts = [];
			failure = '';
		}
	});

	async function confirm() {
		busy = true;
		failure = '';
		try {
			await onsplit(groups);
			open = false;
		} catch (e) {
			failure = e instanceof Error ? e.message : 'The document could not be split.';
		} finally {
			busy = false;
		}
	}
</script>

<Modal bind:open title="Split into separate documents" size="lg" {busy}>
	{#snippet body()}
		<Text as="p" tone="muted" size="sm">
			Cut between the pages where one document ends and the next begins. The original is kept, and
			the split can be undone from Activity.
		</Text>
		<ol class="strip">
			{#each { length: pages } as _, i (i)}
				{@const n = i + 1}
				<li class="thumb" class:alt={(groupOf.get(n) ?? 0) % 2 === 1}>
					<img src={pageUrl(id, n, 240, version)} alt="Page {n}" loading="lazy" />
					<Text size="xs" tone="muted" numeric>{n}</Text>
				</li>
				{#if n < pages}
					<li class="cut" class:on={cuts.includes(n)}>
						<IconButton
							label={cuts.includes(n)
								? `Remove the cut after page ${n}`
								: `Cut after page ${n}`}
							variant="ghost"
							box="sm"
							pressed={cuts.includes(n)}
							onclick={() => (cuts = toggleCut(cuts, n))}
						>
							<circle cx="6" cy="6" r="3" /><path d="M8.12 8.12 12 12" /><path
								d="M20 4 8.12 15.88"
							/><circle cx="6" cy="18" r="3" /><path d="M14.8 14.8 20 20" />
						</IconButton>
					</li>
				{/if}
			{/each}
		</ol>
		<Text as="p" size="sm" aria-live="polite">
			{#if groups.length < 2}
				No cut yet: this stays one document.
			{:else}
				{groups.length} documents: {groups.map(describeGroup).join(' · ')}
			{/if}
		</Text>
		{#if failure}
			<Text as="p" tone="danger" size="sm" role="alert">{failure}</Text>
		{/if}
	{/snippet}
	{#snippet footer()}
		<Button onclick={() => (open = false)} disabled={busy}>Cancel</Button>
		<Button variant="primary" disabled={groups.length < 2} loading={busy} onclick={confirm}>
			Split into {Math.max(groups.length, 2)}
		</Button>
	{/snippet}
</Modal>

<style>
	.strip {
		display: flex;
		flex-wrap: wrap;
		align-items: center;
		row-gap: var(--sp-3);
		list-style: none;
		margin: var(--sp-4) 0;
		padding: 0;
	}
	.thumb {
		display: flex;
		flex-direction: column;
		align-items: center;
		gap: var(--sp-1);
		padding: var(--sp-2);
		border-radius: var(--radius-md);
		background: var(--bg-elevated);
	}
	.thumb.alt {
		background: color-mix(in srgb, var(--accent) 14%, var(--bg-elevated));
	}
	.thumb img {
		display: block;
		inline-size: 5.5rem;
		aspect-ratio: 210 / 297;
		object-fit: contain;
		background: var(--bg);
		border: 1px solid var(--border);
		border-radius: var(--radius-sm);
	}
	.cut {
		display: flex;
		align-items: center;
		align-self: stretch;
		padding-inline: 1px;
	}
	.cut.on {
		padding-inline: var(--sp-3);
	}
</style>
