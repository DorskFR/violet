<script lang="ts">
	import {
		Button,
		Callout,
		Field,
		IconButton,
		Input,
		KeyValue,
		type KeyValueRow,
		Link,
		SectionHeader,
		Stack,
		Text,
		Timestamp,
	} from '@dorsk/tsumikit';
	import { resolve } from '$app/paths';
	import { type Document, type DocumentPatch, type Facets, fileUrl } from '#lib/api';
	import { fileName, formatAmount, formatBytes, formatDate, pageCount } from '#lib/format';
	import { type Draft, type DraftErrors, draftOf, factLabel, patchOf } from '#lib/metadata';
	import { EMPTY_FILTERS, filtersHref } from '#lib/query';

	let {
		doc,
		facets,
		onsave,
	}: {
		doc: Document;
		facets: Facets | null;
		onsave: (patch: DocumentPatch) => Promise<void>;
	} = $props();

	const uid = $props.id();
	const sidecar = $derived(doc.sidecar);

	let draft = $state<Draft | null>(null);
	let errors = $state<DraftErrors>({});
	let saving = $state(false);
	let failure = $state('');

	const search = (patch: Partial<typeof EMPTY_FILTERS>) =>
		filtersHref({ ...EMPTY_FILTERS, ...patch }, resolve('/'));

	function edit() {
		draft = draftOf(sidecar);
		errors = {};
		failure = '';
	}

	async function save(e: SubmitEvent) {
		e.preventDefault();
		if (!draft) return;
		const result = patchOf(sidecar, draft);
		if (result.errors) {
			errors = result.errors;
			return;
		}
		errors = {};
		if (Object.keys(result.patch).length === 0) {
			draft = null;
			return;
		}
		saving = true;
		failure = '';
		try {
			await onsave(result.patch);
			draft = null;
		} catch (err) {
			failure = err instanceof Error ? err.message : 'The change could not be saved.';
		} finally {
			saving = false;
		}
	}

	const rows = $derived<KeyValueRow[]>([
		{ label: 'Area', value: areaValue },
		{ label: 'Type', value: typeValue },
		...(sidecar.issuer ? [{ label: 'Issuer', value: issuerValue }] : []),
		{ label: 'Date', value: sidecar.date ? formatDate(sidecar.date) : 'Not dated' },
		{ label: 'Pages', value: pageCount(sidecar.pages.length) },
		{ label: 'File', value: fileValue },
		{ label: 'Filed', value: filedValue },
	]);
</script>

{#snippet areaValue()}
	<Link href={search({ area: sidecar.area })}>{sidecar.area}</Link>
{/snippet}
{#snippet typeValue()}
	<Link href={search({ type: sidecar.doc_type })}>{sidecar.doc_type}</Link>
{/snippet}
{#snippet issuerValue()}
	<Link href={search({ issuer: sidecar.issuer ?? '' })}>{sidecar.issuer}</Link>
{/snippet}
{#snippet fileValue()}
	<span class="file">
		<Link href={fileUrl(sidecar.id)} target="_blank" rel="noopener">Open</Link>
		<Link href={fileUrl(sidecar.id)} download={fileName(doc.path)}>Download</Link>
		<Text tone="muted" size="sm" numeric>{formatBytes(doc.size)}</Text>
	</span>
{/snippet}
{#snippet filedValue()}
	<Timestamp value={sidecar.created_at} mode="relative" />
{/snippet}

<section aria-label="Details">
	{#if draft}
		<form onsubmit={save} novalidate>
			<Stack gap="var(--sp-3)">
				<SectionHeader title="Edit details" level={2} size="sm" />
				<Field label="Title" for="{uid}-title" error={errors.title}>
					<Input id="{uid}-title" bind:value={draft.title} autocomplete="off" />
				</Field>
				<div class="pair">
					<Field label="Area" for="{uid}-area" error={errors.area}>
						<Input id="{uid}-area" bind:value={draft.area} list="{uid}-areas" autocomplete="off" />
					</Field>
					<Field label="Type" for="{uid}-type" error={errors.doc_type}>
						<Input
							id="{uid}-type"
							bind:value={draft.doc_type}
							list="{uid}-types"
							autocomplete="off"
						/>
					</Field>
				</div>
				<div class="pair">
					<Field label="Issuer" for="{uid}-issuer">
						<Input
							id="{uid}-issuer"
							bind:value={draft.issuer}
							list="{uid}-issuers"
							autocomplete="off"
						/>
					</Field>
					<Field label="Date" for="{uid}-date" error={errors.date}>
						<Input id="{uid}-date" type="date" bind:value={draft.date} />
					</Field>
				</div>
				<Field label="Tags" for="{uid}-tags" hint="Separated by commas.">
					<Input id="{uid}-tags" bind:value={draft.tags} autocomplete="off" />
				</Field>

				<Stack gap="var(--sp-2)">
					<Text variant="label" tone="muted">Facts</Text>
					{#each draft.facts as fact, i (i)}
						<div class="fact-edit" role="group" aria-label="Fact {i + 1}">
							<Input
								bind:value={fact.key}
								placeholder="total"
								aria-label="Fact {i + 1} name"
								autocomplete="off"
							/>
							<Input
								bind:value={fact.value}
								placeholder="as printed"
								aria-label="Fact {i + 1} value as printed"
								autocomplete="off"
							/>
							<Input
								bind:value={fact.amount}
								placeholder="amount"
								inputmode="decimal"
								aria-label="Fact {i + 1} amount"
								autocomplete="off"
							/>
							<Input
								bind:value={fact.currency}
								placeholder="JPY"
								maxlength={3}
								list="{uid}-currencies"
								aria-label="Fact {i + 1} currency"
								autocomplete="off"
							/>
							<IconButton
								icon="x"
								label="Remove fact {i + 1}"
								variant="ghost"
								box="sm"
								type="button"
								onclick={() => draft?.facts.splice(i, 1)}
							/>
							{#if errors.facts?.[i]}
								<Text tone="danger" size="sm" class="fact-error" role="alert">
									{errors.facts[i]}
								</Text>
							{/if}
						</div>
					{/each}
					<div>
						<Button
							size="sm"
							variant="ghost"
							type="button"
							onclick={() => draft?.facts.push({ key: '', value: '', amount: '', currency: '' })}
						>
							Add a fact
						</Button>
					</div>
				</Stack>

				{#if failure}
					<Callout tone="danger" title="Not saved">{failure}</Callout>
				{/if}
				<div class="buttons">
					<Button type="button" disabled={saving} onclick={() => (draft = null)}>Cancel</Button>
					<Button type="submit" variant="primary" loading={saving}>Save</Button>
				</div>
			</Stack>
			<datalist id="{uid}-areas">
				{#each facets?.areas ?? [] as f (f.value)}<option value={f.value}></option>{/each}
			</datalist>
			<datalist id="{uid}-types">
				{#each facets?.doc_types ?? [] as f (f.value)}<option value={f.value}></option>{/each}
			</datalist>
			<datalist id="{uid}-issuers">
				{#each facets?.issuers ?? [] as f (f.value)}<option value={f.value}></option>{/each}
			</datalist>
			<datalist id="{uid}-currencies">
				{#each ['JPY', 'USD', 'EUR', 'GBP'] as c (c)}<option value={c}></option>{/each}
			</datalist>
		</form>
	{:else}
		<Stack gap="var(--sp-4)">
			<SectionHeader title="Details" level={2} size="sm">
				{#snippet actions()}
					<Button size="sm" onclick={edit}>Edit</Button>
				{/snippet}
			</SectionHeader>
			{#if sidecar.summary}
				<Text as="p" tone="muted" size="sm">{sidecar.summary}</Text>
			{/if}
			<KeyValue {rows} dense />
			{#if sidecar.tags.length}
				<div class="tags" aria-label="Tags">
					{#each sidecar.tags as tag (tag)}
						<Link href={search({ tags: [tag] })} tone="muted">#{tag}</Link>
					{/each}
				</div>
			{/if}
			{#if sidecar.facts.length}
				<Stack gap="var(--sp-2)">
					<Text variant="label" tone="muted">Facts</Text>
					<dl class="facts">
						{#each sidecar.facts as fact, i (i)}
							<dt>{factLabel(fact.key)}</dt>
							<dd title={fact.value}>
								{#if fact.amount_minor !== null}
									<Text numeric weight="medium">
										{formatAmount(fact.amount_minor, fact.currency)}
									</Text>
								{:else}
									{fact.value}
								{/if}
							</dd>
						{/each}
					</dl>
				</Stack>
			{/if}
			<Text as="p" tone="faint" size="xs" mono wrap="anywhere">{doc.path}</Text>
		</Stack>
	{/if}
</section>

<style>
	.pair {
		display: grid;
		grid-template-columns: repeat(2, minmax(0, 1fr));
		gap: var(--sp-2);
	}
	.fact-edit {
		display: grid;
		grid-template-columns: minmax(0, 1fr) minmax(0, 1.2fr) minmax(0, 1fr) 4.75rem auto;
		gap: var(--sp-1);
		align-items: center;
	}
	.fact-edit :global(.fact-error) {
		grid-column: 1 / -1;
	}
	.buttons {
		display: flex;
		justify-content: flex-end;
		gap: var(--sp-2);
	}
	.file {
		display: inline-flex;
		flex-wrap: wrap;
		align-items: baseline;
		gap: var(--sp-3);
	}
	.tags {
		display: flex;
		flex-wrap: wrap;
		gap: var(--sp-1) var(--sp-3);
		font-size: var(--fs-sm);
	}
	.facts {
		display: grid;
		grid-template-columns: auto minmax(0, 1fr);
		gap: var(--sp-1) var(--sp-4);
		margin: 0;
		font-size: var(--fs-sm);
	}
	.facts dt {
		color: var(--text-muted);
	}
	.facts dd {
		margin: 0;
		text-align: end;
		overflow-wrap: anywhere;
	}
	@media (max-width: 39.999rem) {
		.fact-edit {
			grid-template-columns: minmax(0, 1fr) 4.75rem auto;
			padding-block-end: var(--sp-3);
		}
		.fact-edit > :global(:nth-child(-n + 2)) {
			grid-column: 1 / -1;
		}
	}
</style>
