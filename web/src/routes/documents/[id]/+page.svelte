<script lang="ts">
	import {
		Button,
		ConfirmModal,
		EmptyState,
		Heading,
		IconButton,
		Link,
		Menu,
		type MenuItem,
		Skeleton,
		Text,
		toasts,
	} from '@dorsk/tsumikit';
	import { goto } from '$app/navigation';
	import { resolve } from '$app/paths';
	import { page } from '$app/state';
	import {
		ApiError,
		api,
		type Document,
		type DocumentPatch,
		errorMessage,
		type Facets,
		fileUrl,
	} from '#lib/api';
	import MergeDialog from '#lib/components/MergeDialog.svelte';
	import MetadataPanel from '#lib/components/MetadataPanel.svelte';
	import PageView from '#lib/components/PageView.svelte';
	import SplitDialog from '#lib/components/SplitDialog.svelte';
	import { fileName, formatDate } from '#lib/format';

	const id = $derived(page.params.id ?? '');

	let doc = $state<Document | null>(null);
	let facets = $state<Facets | null>(null);
	let problem = $state<{ missing: boolean; message: string } | null>(null);
	let textPages = $state<number[]>([]);
	let busy = $state(false);
	let splitting = $state(false);
	let merging = $state(false);
	let deleting = $state(false);
	let deletingPage = $state<number | null>(null);
	let confirmPage = $state(false);

	const sidecar = $derived(doc?.sidecar);
	const pages = $derived(sidecar?.pages ?? []);
	const version = $derived(sidecar?.sha256.slice(0, 12) ?? '');
	const allText = $derived(pages.length > 0 && textPages.length === pages.length);

	async function load(target: string) {
		doc = null;
		problem = null;
		textPages = [];
		try {
			const loaded = await api.document(target);
			if (target === id) doc = loaded;
		} catch (e) {
			if (target !== id) return;
			problem = {
				missing: e instanceof ApiError && e.status === 404,
				message: errorMessage(e, 'The document could not be loaded.'),
			};
		}
	}

	$effect(() => {
		load(id);
	});

	$effect(() => {
		api
			.facets()
			.then((f) => {
				facets = f;
			})
			.catch(() => {});
	});

	async function save(patch: DocumentPatch) {
		doc = await api.update(id, patch);
		toasts.ok('Saved');
		api
			.facets()
			.then((f) => {
				facets = f;
			})
			.catch(() => {});
	}

	async function run(action: () => Promise<Document>, done: string) {
		busy = true;
		try {
			doc = await action();
			textPages = textPages.filter((n) => n <= (doc?.sidecar.pages.length ?? 0));
			toasts.ok(done);
		} catch (e) {
			toasts.error(errorMessage(e, 'The change could not be made.'));
		} finally {
			busy = false;
		}
	}

	const rotate = (numbers: number[], degrees: 90 | 270) =>
		run(
			() => api.rotate(id, numbers, degrees),
			numbers.length === 1 ? `Page ${numbers[0]} rotated` : 'All pages rotated',
		);

	async function deletePage() {
		const n = deletingPage;
		if (n === null) return;
		doc = await api.deletePages(id, [n]);
		textPages = [];
		toasts.ok(`Page ${n} deleted`);
	}

	async function split(groups: number[][]) {
		const { documents } = await api.split(id, groups);
		toasts.ok(`Split into ${documents.length} documents`);
		const first = documents[0];
		await goto(first ? resolve('/documents/[id]', { id: first }) : resolve('/'));
	}

	async function merge(ids: string[]) {
		const merged = await api.merge(ids);
		toasts.ok(`Merged ${ids.length} documents`);
		await goto(resolve('/documents/[id]', { id: merged.sidecar.id }));
	}

	async function remove() {
		await api.remove(id);
		toasts.show('Document deleted', {
			tone: 'ok',
			action: { label: 'Activity', run: () => goto(resolve('/activity')) },
		});
		await goto(resolve('/'));
	}

	function download() {
		if (!doc) return;
		const a = document.createElement('a');
		a.href = fileUrl(id);
		a.download = fileName(doc.path);
		a.click();
	}

	function toggleText(n: number) {
		textPages = textPages.includes(n) ? textPages.filter((p) => p !== n) : [...textPages, n];
	}

	const menu = $derived<MenuItem[]>([
		{ label: 'Split into documents…', icon: 'fork', disabled: pages.length < 2, onselect: () => {
				splitting = true;
			} },
		{ label: 'Merge with others…', icon: 'git-merge', onselect: () => {
				merging = true;
			} },
		{
			label: 'Rotate all pages right',
			icon: 'retry',
			disabled: busy,
			onselect: () => {
				rotate(
					pages.map((p) => p.number),
					90,
				);
			},
		},
		{ label: 'Download', icon: 'download', onselect: download },
		{ label: 'Delete document…', icon: 'trash', danger: true, onselect: () => {
				deleting = true;
			} },
	]);
</script>

<svelte:head>
	<title>{sidecar ? `${sidecar.title} · violet` : 'violet'}</title>
</svelte:head>

<div class="page">
	{#if problem}
		<EmptyState
			tone="danger"
			icon="alert-circle"
			title={problem.missing ? 'This document is not in the library' : 'The document did not load'}
			description={problem.missing
				? 'It may have been deleted, split or merged. Activity shows what happened to it.'
				: `${problem.message} Check that the violet server is up, then try again.`}
		>
			{#snippet action()}
				{#if problem?.missing}
					<Button variant="primary" href={resolve('/activity')}>Open Activity</Button>
				{:else}
					<Button variant="primary" onclick={() => load(id)}>Try again</Button>
				{/if}
				<Button href={resolve('/')}>Back to search</Button>
			{/snippet}
		</EmptyState>
	{:else}
		<header class="top">
			<IconButton
				icon="back"
				label="Back to search"
				variant="ghost"
				onclick={() => (history.length > 1 ? history.back() : goto(resolve('/')))}
			/>
			<div class="titles">
				{#if sidecar}
					<Heading level={1} size="lg" wrap="anywhere">{sidecar.title}</Heading>
					<Text tone="muted" size="sm">
						{[sidecar.issuer, formatDate(sidecar.date)].filter(Boolean).join(' · ')}
					</Text>
				{:else}
					<Skeleton width="60%" height="1.75rem" />
					<Skeleton width="30%" />
				{/if}
			</div>
			{#if sidecar}
				<div class="actions">
					<Button
						control
						pressed={allText}
						onclick={() => (textPages = allText ? [] : pages.map((p) => p.number))}
					>
						Text
					</Button>
					<Button control href={fileUrl(id)} target="_blank" rel="noopener">Open file</Button>
					<Menu label="More actions" items={menu} placement="bottom-end" variant="default" control>
						{#snippet trigger()}More{/snippet}
					</Menu>
				</div>
			{/if}
		</header>

		<div class="body">
			<div class="sheets">
				{#if doc && sidecar}
					{#each pages as p (p.number)}
						<PageView
							{id}
							page={p}
							total={pages.length}
							{version}
							showText={textPages.includes(p.number)}
							{busy}
							onrotate={(degrees) => rotate([p.number], degrees)}
							ondelete={() => {
								deletingPage = p.number;
								confirmPage = true;
							}}
							ontext={() => toggleText(p.number)}
						/>
					{/each}
				{:else}
					<Skeleton aspect="210/297" />
				{/if}
			</div>
			<aside class="panel">
				{#if doc}
					<MetadataPanel {doc} {facets} onsave={save} />
				{:else}
					<Skeleton lines={8} />
				{/if}
			</aside>
		</div>
	{/if}
</div>

{#if doc && sidecar}
	<SplitDialog bind:open={splitting} {id} pages={pages.length} {version} onsplit={split} />
	<MergeDialog bind:open={merging} {id} title={sidecar.title} onmerge={merge} />
	<ConfirmModal
		bind:open={deleting}
		title="Delete this document?"
		message={`"${sidecar.title}" leaves the library and the search. The file is kept in the originals folder, and Activity can bring the document back.`}
		tone="danger"
		confirmLabel="Delete document"
		onconfirm={remove}
	/>
	<ConfirmModal
		bind:open={confirmPage}
		title={`Delete page ${deletingPage}?`}
		message="The page is removed from this document. The file as it is now is kept in the originals folder, and Activity can undo this."
		tone="danger"
		confirmLabel="Delete page"
		onconfirm={deletePage}
	/>
{/if}

<style>
	.top {
		display: flex;
		align-items: flex-start;
		gap: var(--sp-2);
	}
	.titles {
		display: flex;
		flex-direction: column;
		gap: var(--sp-1);
		flex: 1;
		min-inline-size: 0;
	}
	.actions {
		display: flex;
		gap: var(--sp-2);
		flex: none;
	}
	.body {
		display: grid;
		grid-template-columns: minmax(0, 1fr);
		gap: var(--sp-6);
		align-items: start;
	}
	.sheets {
		display: flex;
		flex-direction: column;
		gap: var(--sp-6);
		min-inline-size: 0;
		order: 2;
	}
	.panel {
		min-inline-size: 0;
	}
	@media (max-width: 39.999rem) {
		.top {
			flex-wrap: wrap;
		}
		.actions {
			inline-size: 100%;
			justify-content: flex-end;
		}
	}
	@media (min-width: 64rem) {
		.body {
			grid-template-columns: minmax(0, 1fr) 23rem;
			column-gap: var(--sp-8);
		}
		.sheets {
			order: 0;
			max-inline-size: 52rem;
		}
		.panel {
			position: sticky;
			top: calc(var(--shell-header-h, var(--header-h)) + var(--sp-4));
			max-block-size: calc(100dvh - var(--shell-header-h, var(--header-h)) - var(--sp-8));
			overflow-y: auto;
		}
	}
</style>
