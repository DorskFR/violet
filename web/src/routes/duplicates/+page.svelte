<script lang="ts">
	import {
		Artwork,
		Badge,
		Button,
		ConfirmModal,
		EmptyState,
		formatTimestamp,
		Heading,
		KeyValue,
		type KeyValueRow,
		Link,
		Skeleton,
		Stack,
		Text,
		Timestamp,
		toasts,
	} from '@dorsk/tsumikit';
	import { untrack } from 'svelte';
	import { resolve } from '$app/paths';
	import {
		api,
		type DuplicateCopy,
		type DuplicatePair,
		errorMessage,
		type Hit,
		pageSrcset,
		pageUrl,
	} from '#lib/api';
	import {
		differences,
		documentCount,
		type Field,
		hasPages,
		origins,
		pairKey,
		sameFile,
		withoutDocument,
		withoutPair,
	} from '#lib/duplicates';
	import { formatBytes, formatDate, pageCount } from '#lib/format';
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

	function rows(copy: DuplicateCopy, differs: Field[], filed: KeyValueRow['value']): KeyValueRow[] {
		const tone = (field: Field) => (differs.includes(field) ? ('warn' as const) : undefined);
		return [
			{ label: 'Filed', value: filed },
			{ label: 'From', value: origins(copy).join(', ') || 'Unknown' },
			{ label: 'Size', value: copy.size === null ? 'Unknown' : formatBytes(copy.size), tone: tone('size') },
			{ label: 'Pages', value: pageCount(copy.pages), tone: tone('pages') },
			{ label: 'Area', value: copy.area, tone: tone('area') },
			{ label: 'Issuer', value: copy.issuer ?? 'None', tone: tone('issuer') },
			{ label: 'Date', value: formatDate(copy.date) || 'None', tone: tone('date') },
		];
	}
</script>

<svelte:head>
	<title>Possible duplicates · violet</title>
</svelte:head>

{#snippet side(copy: DuplicateCopy, other: DuplicateCopy, differs: Field[], pending: boolean)}
	{#snippet filed()}
		<span title={formatTimestamp(copy.created_at, 'datetime')}>
			<Timestamp value={copy.created_at} mode="relative" tone="inherit" />
		</span>
	{/snippet}
	<div class="side">
		<a class="thumb" href={resolve('/documents/[id]', { id: copy.id })} aria-label={`Open ${copy.title}`}>
			<Artwork
				src={hasPages(copy) ? pageUrl(copy.id, 1, 240) : undefined}
				srcset={hasPages(copy) ? pageSrcset(copy.id, 1, [240, 480]) : undefined}
				sizes="8rem"
				alt=""
				seed={copy.title}
				aspect="210/297"
				size="8rem"
				radius="sm"
				fallback="icon"
				icon="file-text"
			/>
		</a>
		<div class="details">
			<Text weight="medium" wrap="anywhere" tone={differs.includes('title') ? 'warn' : undefined}>
				<Link href={resolve('/documents/[id]', { id: copy.id })}>{copy.title}</Link>
			</Text>
			<Text tone="muted" size="sm" wrap="anywhere">{copy.path}</Text>
			<KeyValue dense rows={rows(copy, differs, filed)} />
			<div>
				<Button size="sm" disabled={pending} onclick={() => keep(copy, other)}>Keep this one</Button>
			</div>
		</div>
	</div>
{/snippet}

<div class="page narrow">
	<Heading level={1} size="lg">Possible duplicates</Heading>
	<Text as="p" tone="muted" size="sm">
		The same file filed twice, or documents with the same issuer, date and type whose text and
		numbers match. Differences between the two copies are highlighted. Nothing is merged or
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
				{@const differs = differences(a, b)}
				<li class="pair">
					<div class="sides">
						{@render side(a, b, differs, pending)}
						{@render side(b, a, differs, pending)}
					</div>
					<div class="foot">
						<div class="verdict">
							{#if sameFile(pair)}
								<Badge size="sm" tone="warn" icon="copy">Identical file</Badge>
							{:else}
								<Badge size="sm" tone="warn" icon="copy">Possible duplicate</Badge>
							{/if}
							<Text tone="muted" size="sm">
								{differs.length
									? `Differ in ${differs.join(', ')}`
									: 'Same title, area, issuer, date, pages and size'}
							</Text>
						</div>
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
		title="Delete the other copy?"
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
		flex-wrap: wrap;
		align-items: center;
		justify-content: space-between;
		gap: var(--sp-2);
	}
	.verdict {
		display: flex;
		flex-wrap: wrap;
		align-items: center;
		gap: var(--sp-2);
		min-inline-size: 0;
	}
	@media (min-width: 48rem) {
		.sides {
			grid-template-columns: repeat(2, minmax(0, 1fr));
		}
	}
</style>
