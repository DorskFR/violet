<script lang="ts">
	import {
		Badge,
		Button,
		ConfirmModal,
		EmptyState,
		Heading,
		Icon,
		type IconName,
		Link,
		Skeleton,
		Text,
		Timestamp,
		toasts,
	} from '@dorsk/tsumikit';
	import { resolve } from '$app/paths';
	import { ACTION_LABEL, actorLabel, affectedDocuments } from '#lib/activity';
	import { type Action, type ActivityItem, api, errorMessage } from '#lib/api';

	const ICON: Record<Action['kind'], IconName> = {
		ingest: 'file-text',
		duplicate: 'copy',
		split: 'fork',
		merge: 'git-merge',
		edit_pages: 'image',
		update: 'edit',
		delete: 'trash',
		replace: 'recycle',
		not_duplicate: 'check',
		settings: 'settings',
		undo: 'retry',
	};
	const STEP = 50;

	let items = $state<ActivityItem[] | null>(null);
	let error = $state('');
	let limit = $state(STEP);
	let loadingMore = $state(false);
	let target = $state<ActivityItem | null>(null);
	let confirming = $state(false);

	async function load() {
		error = '';
		try {
			items = await api.activity(limit);
		} catch (e) {
			error = errorMessage(e, 'The activity could not be loaded.');
		}
	}

	$effect(() => {
		load();
	});

	async function more() {
		loadingMore = true;
		limit += STEP;
		await load();
		loadingMore = false;
	}

	async function undo() {
		if (!target) return;
		await api.undo(target.entry.id);
		toasts.ok('Undone');
		await load();
	}
</script>

<svelte:head>
	<title>Activity · violet</title>
</svelte:head>

<div class="page narrow">
	<Heading level={1} size="lg">Activity</Heading>
	{#if error && !items}
		<EmptyState
			tone="danger"
			icon="alert-circle"
			title="The activity did not load"
			description="{error} Check that the violet server is up, then try again."
			actionLabel="Try again"
			onAction={load}
		/>
	{:else if !items}
		<Skeleton lines={10} />
	{:else if items.length === 0}
		<EmptyState
			icon="clock"
			title="Nothing has happened yet"
			description="Every file violet reads and every change you make is listed here, and can be undone from here."
		>
			{#snippet action()}
				<Button href={resolve('/status')}>Upload files</Button>
			{/snippet}
		</EmptyState>
	{:else}
		<ol class="plain-list feed">
			{#each items as item (item.entry.id)}
				{@const entry = item.entry}
				{@const docs = affectedDocuments(entry.action)}
				<li class:undone={item.undone}>
					<span class="glyph"><Icon name={ICON[entry.action.kind]} size={16} /></span>
					<div class="main">
						<Text class="summary" wrap="anywhere">{entry.summary}</Text>
						<div class="meta">
							<Badge size="xs" tone={entry.actor === 'agent' ? 'violet' : 'muted'}>
								{actorLabel(entry)}
							</Badge>
							<Text size="sm" tone="muted">{ACTION_LABEL[entry.action.kind]}</Text>
							<Timestamp value={entry.at} mode="relative" size="sm" tone="muted" />
							{#if item.undone}<Badge size="xs" tone="warn" icon="retry">undone</Badge>{/if}
						</div>
						{#if docs.length}
							<ul class="plain-list docs">
								{#each docs as d (d.id)}
									<li>
										{#if d.gone !== item.undone}
											<Text size="sm" tone="faint" wrap="anywhere">{d.label}</Text>
										{:else}
											<Link href={resolve('/documents/[id]', { id: d.id })}>
												<Text size="sm" wrap="anywhere">{d.label}</Text>
											</Link>
										{/if}
									</li>
								{/each}
							</ul>
						{/if}
					</div>
					{#if item.undoable}
						<Button
							size="sm"
							onclick={() => {
								target = item;
								confirming = true;
							}}
							aria-label="Undo: {entry.summary}"
						>
							Undo
						</Button>
					{/if}
				</li>
			{/each}
		</ol>
		{#if items.length >= limit}
			<div class="more">
				<Button onclick={more} loading={loadingMore}>Show older activity</Button>
			</div>
		{/if}
	{/if}
</div>

<ConfirmModal
	bind:open={confirming}
	title="Undo this change?"
	message={target
		? `"${target.entry.summary}" is reversed: the library goes back to how it was before it. The undo itself is listed in Activity.`
		: ''}
	confirmLabel="Undo"
	onconfirm={undo}
/>

<style>
	.narrow {
		max-inline-size: 56rem;
	}
	.feed > li {
		display: grid;
		grid-template-columns: auto minmax(0, 1fr) auto;
		gap: var(--sp-3);
		align-items: start;
		padding-block: var(--sp-3);
	}
	.feed > li + li {
		border-block-start: 1px solid var(--border);
	}
	.glyph {
		display: grid;
		place-items: center;
		inline-size: 1.75rem;
		block-size: 1.75rem;
		border-radius: var(--radius-md);
		background: var(--bg-elevated);
		color: var(--text-muted);
	}
	.main {
		display: flex;
		flex-direction: column;
		gap: var(--sp-1);
		min-inline-size: 0;
	}
	.undone :global(.summary) {
		color: var(--text-muted);
		text-decoration: line-through;
	}
	.meta {
		display: flex;
		flex-wrap: wrap;
		align-items: center;
		gap: var(--sp-2);
	}
	.docs {
		display: flex;
		flex-direction: column;
	}
	.more {
		display: flex;
		justify-content: center;
	}
</style>
