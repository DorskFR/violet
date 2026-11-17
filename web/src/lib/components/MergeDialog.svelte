<script lang="ts">
	import { Badge, Button, Checkbox, Input, Modal, Text } from '@dorsk/tsumikit';
	import { api, type Hit } from '#lib/api';
	import { formatDate, pageCount } from '#lib/format';
	import Snippet from './Snippet.svelte';

	let {
		open = $bindable(false),
		id,
		title,
		onmerge,
	}: {
		open: boolean;
		id: string;
		title: string;
		onmerge: (ids: string[]) => Promise<void>;
	} = $props();

	let text = $state('');
	let hits = $state<Hit[]>([]);
	let loading = $state(false);
	let picked = $state<Hit[]>([]);
	let busy = $state(false);
	let failure = $state('');
	let request = 0;

	$effect(() => {
		if (open) {
			text = '';
			picked = [];
			failure = '';
		}
	});

	$effect(() => {
		if (!open) return;
		const q = text;
		const run = ++request;
		loading = true;
		const timer = setTimeout(
			async () => {
				try {
					const result = await api.search({ q, limit: 12 });
					if (run === request) hits = result.hits.filter((h) => h.id !== id);
				} catch (e) {
					if (run === request) {
						hits = [];
						failure = e instanceof Error ? e.message : 'The search could not be run.';
					}
				} finally {
					if (run === request) loading = false;
				}
			},
			q ? 200 : 0,
		);
		return () => clearTimeout(timer);
	});

	const isPicked = (hit: Hit) => picked.some((p) => p.id === hit.id);

	function toggle(hit: Hit) {
		picked = isPicked(hit) ? picked.filter((p) => p.id !== hit.id) : [...picked, hit];
	}

	async function confirm() {
		busy = true;
		failure = '';
		try {
			await onmerge([id, ...picked.map((p) => p.id)]);
			open = false;
		} catch (e) {
			failure = e instanceof Error ? e.message : 'The documents could not be merged.';
		} finally {
			busy = false;
		}
	}
</script>

<Modal bind:open title="Merge with other documents" size="lg" {busy}>
	{#snippet body()}
		<Text as="p" tone="muted" size="sm">
			The pages of the documents you pick are added after "{title}", in the order you pick them.
			The originals are kept, and the merge can be undone from Activity.
		</Text>
		<div class="search">
			<Input
				enterkeyhint="search"
				icon="search"
				grow
				clearable
				autocomplete="off"
				placeholder="Find the documents to add"
				aria-label="Find documents to merge"
				bind:value={text}
			/>
		</div>
		{#if picked.length}
			<div class="picked" aria-label="Picked documents">
				{#each picked as hit, i (hit.id)}
					<Badge
						size="sm"
						tone="violet"
						truncate
						maxWidth="16rem"
						removable
						actionLabel="Remove {hit.title}"
						onremove={() => toggle(hit)}
					>
						{i + 2}. {hit.title}
					</Badge>
				{/each}
			</div>
		{/if}
		<ul class="plain-list hits" aria-busy={loading}>
			{#each hits as hit (hit.id)}
				<li>
					<Checkbox
						label="Add {hit.title}"
						labelHidden
						checked={isPicked(hit)}
						onchange={() => toggle(hit)}
						id="merge-{hit.id}"
					/>
					<label for="merge-{hit.id}">
						<Text weight="medium">{hit.title}</Text>
						<Text tone="muted" size="sm">
							{[hit.issuer, formatDate(hit.date), hit.area, pageCount(hit.pages)]
								.filter(Boolean)
								.join(' · ')}
						</Text>
						{#if hit.snippet}
							<Text tone="muted" size="sm" truncate><Snippet html={hit.snippet} /></Text>
						{/if}
					</label>
				</li>
			{:else}
				<li>
					<Text tone="muted" size="sm">
						{loading ? 'Searching…' : 'No other document matches these words.'}
					</Text>
				</li>
			{/each}
		</ul>
		{#if failure}
			<Text as="p" tone="danger" size="sm" role="alert">{failure}</Text>
		{/if}
	{/snippet}
	{#snippet footer()}
		<Button onclick={() => (open = false)} disabled={busy}>Cancel</Button>
		<Button variant="primary" disabled={picked.length === 0} loading={busy} onclick={confirm}>
			{picked.length ? `Merge ${picked.length + 1} documents` : 'Merge'}
		</Button>
	{/snippet}
</Modal>

<style>
	.search {
		display: flex;
		margin-block: var(--sp-3);
	}
	.picked {
		display: flex;
		flex-wrap: wrap;
		gap: var(--sp-1);
		margin-block-end: var(--sp-3);
	}
	.hits {
		display: flex;
		flex-direction: column;
		min-block-size: 12rem;
	}
	.hits li {
		display: grid;
		grid-template-columns: auto minmax(0, 1fr);
		gap: var(--sp-3);
		align-items: start;
		padding-block: var(--sp-2);
	}
	.hits li + li {
		border-block-start: 1px solid var(--border);
	}
	.hits label {
		display: flex;
		flex-direction: column;
		min-inline-size: 0;
		cursor: pointer;
	}
</style>
