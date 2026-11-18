<script lang="ts">
	import {
		Artwork,
		Badge,
		Button,
		ConfirmModal,
		EmptyState,
		Heading,
		Link,
		Skeleton,
		Stack,
		Text,
		toasts,
	} from '@dorsk/tsumikit';
	import { untrack } from 'svelte';
	import { resolve } from '$app/paths';
	import { api, type DuplicatePair, errorMessage, type Hit, pageSrcset, pageUrl } from '#lib/api';
	import { documentCount, hasPages, pairKey, withoutDocument, withoutPair } from '#lib/duplicates';
	import { formatDate, pageCount } from '#lib/format';
	import { revision } from '#lib/library';
	import { announce } from '#lib/undo';

	let pairs = $state<DuplicatePair[] | null>(null);
	let error = $state('');
	let busy = $state<string[]>([]);
	let choice = $state<{ keep: Hit; drop: Hit } | null>(null);
	let confirming = $state(false);

	async function load() {
		try {
			pairs = (await api.duplicates()).pairs;
			error = '';
		} catch (e) {
			error = errorMessage(e, 'The possible duplicates could not be loaded.');
		}
	}

	$effect(() => {
		$revision;
		untrack(load);
	});

	const undoable = {
		undo: (entry: string) => api.undo(entry),
		show: (message: string, options: Parameters<typeof toasts.show>[1]) =>
			toasts.show(message, options),
	};

	const refresh = () => revision.update((n) => n + 1);

	async function dismiss(pair: DuplicatePair) {
		const [a, b] = pair;
		const key = pairKey(pair);
		busy = [...busy, key];
		try {
			const { entry } = await api.notDuplicate(a.id, b.id);
			pairs = withoutPair(pairs ?? [], a.id, b.id);
			announce(undoable, 'Marked as not duplicates', entry, refresh);
		} catch (e) {
			toasts.error(errorMessage(e, 'The pair could not be dismissed.'));
		} finally {
			busy = busy.filter((k) => k !== key);
		}
	}

	function keep(kept: Hit, other: Hit) {
		choice = { keep: kept, drop: other };
		confirming = true;
	}

	async function removeOther() {
		if (!choice) return;
		const { keep: kept, drop } = choice;
		const { entry } = await api.remove(drop.id);
		pairs = withoutDocument(pairs ?? [], drop.id);
		announce(undoable, `Kept "${kept.title}", deleted the other`, entry, refresh);
	}

	const documents = $derived(pairs ? documentCount(pairs) : 0);
</script>

<svelte:head>
	<title>Possible duplicates · violet</title>
</svelte:head>

{#snippet side(hit: Hit, other: Hit, pending: boolean)}
	<div class="side">
		<a class="thumb" href={resolve('/documents/[id]', { id: hit.id })} aria-label={`Open ${hit.title}`}>
			<Artwork
				src={hasPages(hit) ? pageUrl(hit.id, 1, 240) : undefined}
				srcset={hasPages(hit) ? pageSrcset(hit.id, 1, [240, 480]) : undefined}
				sizes="8rem"
				alt=""
				seed={hit.title}
				aspect="210/297"
				size="8rem"
				radius="sm"
				fallback="icon"
				icon="file-text"
			/>
		</a>
		<div class="details">
			<Text weight="medium" wrap="anywhere">
				<Link href={resolve('/documents/[id]', { id: hit.id })}>{hit.title}</Link>
			</Text>
			<Text tone="muted" size="sm" wrap="anywhere">{hit.path}</Text>
			<Text tone="muted" size="sm">
				{[hit.issuer, formatDate(hit.date), pageCount(hit.pages)].filter(Boolean).join(' · ')}
			</Text>
			<div>
				<Button size="sm" disabled={pending} onclick={() => keep(hit, other)}>Keep this one</Button>
			</div>
		</div>
	</div>
{/snippet}

<div class="page narrow">
	<Heading level={1} size="lg">Possible duplicates</Heading>
	<Text as="p" tone="muted" size="sm">
		Documents with the same issuer, date and type whose text and numbers match. Nothing is merged or
		deleted unless you choose: keeping one deletes the other, which Activity can bring back.
	</Text>

	{#if error && !pairs}
		<EmptyState
			tone="danger"
			icon="alert-circle"
			title="The possible duplicates did not load"
			description="{error} Check that the violet server is up, then try again."
			actionLabel="Try again"
			onAction={load}
		/>
	{:else if !pairs}
		<Stack gap="var(--sp-3)">
			{#each { length: 3 } as _, i (i)}<Skeleton height="10rem" />{/each}
		</Stack>
	{:else if pairs.length === 0}
		<EmptyState
			icon="check-circle"
			title="No possible duplicates"
			description="Copies of a document filed twice show up here for review."
		/>
	{:else}
		<Text tone="muted" size="sm">
			{pairs.length === 1 ? '1 pair' : `${pairs.length} pairs`} · {documents} documents
		</Text>
		<ul class="plain-list pairs">
			{#each pairs as pair (pairKey(pair))}
				{@const [a, b] = pair}
				{@const pending = busy.includes(pairKey(pair))}
				<li class="pair">
					<div class="sides">
						{@render side(a, b, pending)}
						{@render side(b, a, pending)}
					</div>
					<div class="foot">
						<Badge size="sm" tone="warn" icon="copy">Possible duplicate</Badge>
						<Button size="sm" variant="ghost" loading={pending} onclick={() => dismiss(pair)}>
							Not a duplicate
						</Button>
					</div>
				</li>
			{/each}
		</ul>
	{/if}
</div>

{#if choice}
	<ConfirmModal
		bind:open={confirming}
		title="Keep this one and delete the other?"
		message={`"${choice.drop.title}" (${choice.drop.path}) leaves the library and the search. The file is kept in the originals folder, and Activity can bring it back.`}
		tone="danger"
		confirmLabel="Delete the other"
		onconfirm={removeOther}
	/>
{/if}

<style>
	.narrow {
		max-inline-size: 64rem;
	}
	.pairs {
		display: flex;
		flex-direction: column;
		gap: var(--sp-4);
	}
	.pair {
		display: flex;
		flex-direction: column;
		gap: var(--sp-3);
		padding: var(--sp-3);
		border: 1px solid var(--border);
		border-radius: var(--radius-md);
	}
	.sides {
		display: grid;
		grid-template-columns: minmax(0, 1fr);
		gap: var(--sp-4);
	}
	.side {
		display: grid;
		grid-template-columns: auto minmax(0, 1fr);
		gap: var(--sp-3);
		align-items: start;
	}
	.thumb {
		display: block;
		border-radius: var(--radius-sm);
	}
	.thumb:focus-visible {
		outline: 2px solid var(--accent);
		outline-offset: 2px;
	}
	.details {
		display: flex;
		flex-direction: column;
		gap: var(--sp-1);
		min-inline-size: 0;
	}
	.foot {
		display: flex;
		align-items: center;
		justify-content: space-between;
		gap: var(--sp-2);
	}
	@media (min-width: 48rem) {
		.sides {
			grid-template-columns: repeat(2, minmax(0, 1fr));
		}
	}
</style>
