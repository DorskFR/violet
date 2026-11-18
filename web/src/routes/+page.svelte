<script lang="ts">
	import {
		Badge,
		Button,
		Drawer,
		EmptyState,
		Input,
		LoadMore,
		mq,
		Skeleton,
		Text,
	} from '@dorsk/tsumikit';
	import { untrack } from 'svelte';
	import { goto } from '$app/navigation';
	import { resolve } from '$app/paths';
	import { page } from '$app/state';
	import { api, errorMessage, type Facets, type Hit, type Understood } from '#lib/api';
	import DocumentRow from '#lib/components/DocumentRow.svelte';
	import FacetFilters from '#lib/components/FacetFilters.svelte';
	import { revision } from '#lib/library';
	import {
		activeFilters,
		EMPTY_FILTERS,
		type Filters,
		filtersHref,
		filtersToParams,
		parseFilters,
		toSearchParams,
		understoodChips,
	} from '#lib/query';

	const PAGE_SIZE = 20;
	const DEBOUNCE_MS = 250;

	const filters = $derived(parseFilters(page.url.searchParams));
	const queryKey = $derived(filtersToParams(filters).toString());
	const active = $derived(activeFilters(filters));

	let text = $state(untrack(() => filters.q));
	let searchEl = $state<HTMLInputElement | null>(null);
	let listEl = $state<HTMLElement | null>(null);
	let facets = $state<Facets | null>(null);
	let hits = $state<Hit[]>([]);
	let total = $state<number | null>(null);
	let partial = $state(false);
	let understood = $state<Understood | undefined>(undefined);
	let phase = $state<'loading' | 'idle' | 'more' | 'error'>('loading');
	let error = $state('');
	let drawer = $state(false);
	let request = 0;
	let timer: ReturnType<typeof setTimeout> | undefined;

	const href = (next: Filters) => filtersHref(next, resolve('/'));

	function apply(next: Filters, typing = false) {
		clearTimeout(timer);
		goto(href(next), { replace: typing, reset: false });
	}

	function ontext() {
		clearTimeout(timer);
		timer = setTimeout(() => apply({ ...filters, q: text }, true), DEBOUNCE_MS);
	}

	$effect(() => {
		const q = filters.q;
		if (untrack(() => text.trim()) !== q) text = q;
	});

	async function load(offset: number) {
		const id = ++request;
		phase = offset === 0 ? 'loading' : 'more';
		try {
			const result = await api.search(toSearchParams(filters, PAGE_SIZE, offset));
			if (id !== request) return;
			hits = offset === 0 ? result.hits : [...hits, ...result.hits];
			total = result.total;
			partial = result.partial ?? false;
			understood = result.understood;
			phase = 'idle';
		} catch (e) {
			if (id !== request) return;
			error = errorMessage(e, 'The search could not be run.');
			phase = 'error';
		}
	}

	$effect(() => {
		queryKey;
		$revision;
		untrack(() => load(0));
	});

	$effect(() => {
		$revision;
		api
			.facets()
			.then((f) => {
				facets = f;
			})
			.catch(() => {});
	});

	const hasMore = $derived(total !== null && hits.length < total);
	const moreState = $derived(
		phase === 'more' ? 'loading' : phase === 'error' ? 'error' : hasMore ? 'idle' : 'done',
	);

	function loadMore() {
		if (phase === 'idle' && hasMore) load(hits.length);
	}

	function autoload(node: HTMLElement) {
		const observer = new IntersectionObserver(
			(entries) => {
				if (entries.some((e) => e.isIntersecting)) loadMore();
			},
			{ rootMargin: '600px' },
		);
		$effect(() => {
			hits.length;
			observer.unobserve(node);
			observer.observe(node);
		});
		return () => observer.disconnect();
	}

	function onkeydown(e: KeyboardEvent) {
		const target = e.target as HTMLElement;
		const editing = target.closest('input, textarea, select, [contenteditable="true"]');
		if (e.key === '/' && !editing && !e.metaKey && !e.ctrlKey && !e.altKey) {
			e.preventDefault();
			searchEl?.focus();
			searchEl?.select();
			return;
		}
		if (e.key !== 'ArrowDown' && e.key !== 'ArrowUp') return;
		const rows = [...(listEl?.querySelectorAll<HTMLElement>('[data-result]') ?? [])];
		const at = rows.indexOf(target.closest<HTMLElement>('[data-result]') as HTMLElement);
		if (target === searchEl && e.key === 'ArrowDown') {
			e.preventDefault();
			rows[0]?.focus();
		} else if (at >= 0) {
			e.preventDefault();
			if (e.key === 'ArrowUp' && at === 0) searchEl?.focus();
			else rows[at + (e.key === 'ArrowDown' ? 1 : -1)]?.focus();
		}
	}

	const filtering = $derived(queryKey !== '');
	const read = $derived(understoodChips(understood, filters.q));
	const heading = $derived(
		total === null
			? ''
			: partial
				? `Partial matches: ${total} ${total === 1 ? 'document has' : 'documents have'} some of the words`
				: filtering
					? `${total} ${total === 1 ? 'document' : 'documents'}`
					: `${total} ${total === 1 ? 'document' : 'documents'}, newest first`,
	);
</script>

<svelte:head>
	<title>{filters.q ? `${filters.q} · violet` : 'violet'}</title>
</svelte:head>

<svelte:window {onkeydown} />

<div class="page search">
	{#if mq.lg}
		<aside class="facets" aria-label="Filters">
			<FacetFilters {filters} {facets} onchange={(next) => apply(next)} />
		</aside>
	{/if}
	<section class="results" aria-label="Documents">
		<div class="searchbar">
			<Input
				enterkeyhint="search"
				icon="search"
				grow
				clearable
				autocomplete="off"
				spellcheck={false}
				placeholder="Search titles, issuers and page text"
				aria-label="Search documents"
				aria-keyshortcuts="/"
				bind:value={text}
				bind:el={searchEl}
				oninput={ontext}
				onclear={() => apply({ ...filters, q: '' }, true)}
				onenter={() => apply({ ...filters, q: text }, true)}
			/>
			{#if !mq.lg}
				<Button onclick={() => (drawer = true)} count={active.length || undefined}>Filters</Button>
			{/if}
		</div>

		<div class="summary" aria-live="polite">
			<Text tone="muted" size="sm">{heading}</Text>
			{#each read as chip (chip.key)}
				<Badge
					size="sm"
					tone="violet"
					removable
					title="Read from the search text"
					actionLabel="Search {chip.label} as words"
					onremove={() => apply({ ...filters, q: chip.q })}
				>
					{chip.label}
				</Badge>
			{/each}
			{#each active as f (f.key)}
				<Badge
					size="sm"
					removable
					actionLabel="Remove filter {f.label}"
					onremove={() => apply(f.cleared)}
				>
					{f.label}
				</Badge>
			{/each}
			{#if active.length > 1}
				<Button variant="link" size="sm" href={href({ ...EMPTY_FILTERS, q: filters.q })}>
					Clear filters
				</Button>
			{/if}
		</div>

		{#if phase === 'loading' && hits.length === 0}
			<ul class="plain-list" aria-busy="true" aria-label="Loading documents">
				{#each { length: 8 } as _, i (i)}
					<li class="placeholder">
						<Skeleton width="3.25rem" aspect="210/297" />
						<div><Skeleton lines={3} /></div>
					</li>
				{/each}
			</ul>
		{:else if phase === 'error' && hits.length === 0}
			<EmptyState
				tone="danger"
				icon="alert-circle"
				title="The search did not run"
				description="{error} Check that the violet server is up, then try again."
				actionLabel="Try again"
				onAction={() => load(0)}
			/>
		{:else if hits.length === 0 && filtering}
			<EmptyState
				icon="search"
				title="No document matches"
				description="Try fewer words or remove a filter. Words match from their start, and quotes keep a phrase together."
			>
				{#snippet action()}
					<Button href={resolve('/')}>Show all documents</Button>
				{/snippet}
			</EmptyState>
		{:else if hits.length === 0}
			<EmptyState
				icon="folder"
				title="The library is empty"
				description="Drop scans, PDFs or photos into the inbox and violet reads, names and files them here."
			>
				{#snippet action()}
					<Button href={resolve('/status')}>Upload files</Button>
				{/snippet}
			</EmptyState>
		{:else}
			<ul class="plain-list list" bind:this={listEl} aria-busy={phase === 'loading'}>
				{#each hits as hit (hit.id)}
					<li><DocumentRow {hit} /></li>
				{/each}
			</ul>
			<div {@attach autoload}>
				{#if hasMore || phase === 'error'}
					<LoadMore
						state={moreState}
						onload={() => (phase === 'error' ? load(hits.length) : loadMore())}
						label="Show more documents"
					/>
				{/if}
			</div>
		{/if}
	</section>
</div>

{#if !mq.lg}
	<Drawer bind:open={drawer} side="bottom" title="Filters">
		<FacetFilters {filters} {facets} onchange={(next) => apply(next)} />
		{#snippet footer()}
			<Button href={href({ ...EMPTY_FILTERS, q: filters.q })} disabled={active.length === 0}>
				Clear
			</Button>
			<Button variant="primary" onclick={() => (drawer = false)}>
				Show {total ?? ''} {total === 1 ? 'document' : 'documents'}
			</Button>
		{/snippet}
	</Drawer>
{/if}

<style>
	.search {
		display: grid;
		grid-template-columns: minmax(0, 1fr);
		align-items: start;
	}
	.facets {
		position: sticky;
		top: calc(var(--shell-header-h, var(--header-h)) + var(--sp-4));
		max-block-size: calc(100dvh - var(--shell-header-h, var(--header-h)) - var(--sp-8));
		overflow-y: auto;
	}
	.results {
		display: flex;
		flex-direction: column;
		gap: var(--sp-3);
		min-inline-size: 0;
	}
	.searchbar {
		display: flex;
		gap: var(--sp-2);
	}
	.summary {
		display: flex;
		flex-wrap: wrap;
		align-items: center;
		gap: var(--sp-2);
		min-block-size: 1.75rem;
	}
	.list {
		display: flex;
		flex-direction: column;
	}
	.list > li + li {
		border-block-start: 1px solid var(--border);
	}
	.placeholder {
		display: grid;
		grid-template-columns: auto minmax(0, 1fr);
		gap: var(--sp-3);
		padding: var(--sp-3);
	}
	@media (min-width: 64rem) {
		.search {
			grid-template-columns: 17rem minmax(0, 1fr);
			column-gap: var(--sp-8);
		}
	}
</style>
