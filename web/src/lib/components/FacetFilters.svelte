<script lang="ts">
	import { Field, Input, Select, Stack, Text, Toggle } from '@dorsk/tsumikit';
	import type { Facet, Facets } from '#lib/api';
	import { type Filters, toggleTag, withYear, yearOf } from '#lib/query';

	let {
		filters,
		facets,
		onchange,
	}: { filters: Filters; facets: Facets | null; onchange: (next: Filters) => void } = $props();

	const CURRENCIES = ['JPY', 'USD', 'EUR', 'GBP'];
	const TAG_LIMIT = 16;
	const uid = $props.id();

	function options(list: Facet[] | undefined, any: string, selected: string) {
		const known = list ?? [];
		const out = [
			{ value: '', label: any },
			...known.map((f) => ({ value: f.value, label: `${f.value} (${f.count})` })),
		];
		if (selected && !known.some((f) => f.value === selected)) {
			out.push({ value: selected, label: selected });
		}
		return out;
	}

	const year = $derived(yearOf(filters));
	const tags = $derived.by(() => {
		const top = (facets?.tags ?? []).slice(0, TAG_LIMIT);
		const extra = filters.tags
			.filter((t) => !top.some((f) => f.value === t))
			.map((value) => ({ value, count: 0 }));
		return [...extra, ...top];
	});
	const currencies = $derived([
		{ value: '', label: 'Any' },
		...[...new Set([...CURRENCIES, ...(filters.currency ? [filters.currency] : [])])].map((c) => ({
			value: c,
			label: c,
		})),
	]);

	const set = (patch: Partial<Filters>) => onchange({ ...filters, ...patch });
	const value = (e: Event) => (e.currentTarget as HTMLInputElement | HTMLSelectElement).value;
</script>

<Stack gap="var(--sp-3)">
	<Field label="Area" for="{uid}-area">
		<Select
			id="{uid}-area"
			value={filters.area}
			options={options(facets?.areas, 'Any area', filters.area)}
			onchange={(e) => set({ area: value(e) })}
		/>
	</Field>
	<Field label="Type" for="{uid}-type">
		<Select
			id="{uid}-type"
			value={filters.type}
			options={options(facets?.doc_types, 'Any type', filters.type)}
			onchange={(e) => set({ type: value(e) })}
		/>
	</Field>
	<Field label="Issuer" for="{uid}-issuer">
		<Select
			id="{uid}-issuer"
			value={filters.issuer}
			options={options(facets?.issuers, 'Any issuer', filters.issuer)}
			onchange={(e) => set({ issuer: value(e) })}
		/>
	</Field>
	<Field label="Year" for="{uid}-year">
		<Select
			id="{uid}-year"
			value={year}
			options={options(facets?.years, 'Any year', year)}
			onchange={(e) => onchange(withYear(filters, value(e)))}
		/>
	</Field>
	<div class="pair">
		<Field label="From" for="{uid}-from">
			<Input
				id="{uid}-from"
				type="date"
				value={filters.from}
				max={filters.to || undefined}
				onchange={(e) => set({ from: value(e) })}
			/>
		</Field>
		<Field label="To" for="{uid}-to">
			<Input
				id="{uid}-to"
				type="date"
				value={filters.to}
				min={filters.from || undefined}
				onchange={(e) => set({ to: value(e) })}
			/>
		</Field>
	</div>
	<div class="amount">
		<Field label="Amount from" for="{uid}-min">
			<Input
				id="{uid}-min"
				inputmode="decimal"
				placeholder="0"
				value={filters.min}
				onchange={(e) => set({ min: value(e).trim() })}
			/>
		</Field>
		<Field label="to" for="{uid}-max">
			<Input
				id="{uid}-max"
				inputmode="decimal"
				placeholder="any"
				value={filters.max}
				onchange={(e) => set({ max: value(e).trim() })}
			/>
		</Field>
		<Field label="Currency" for="{uid}-currency">
			<Select
				id="{uid}-currency"
				value={filters.currency}
				options={currencies}
				onchange={(e) => set({ currency: value(e) })}
			/>
		</Field>
	</div>
	{#if tags.length}
		<Stack gap="var(--sp-2)">
			<Text variant="label" tone="muted" id="{uid}-tags">Tags</Text>
			<div class="tags" role="group" aria-labelledby="{uid}-tags">
				{#each tags as tag (tag.value)}
					<Toggle
						pill
						size="sm"
						pressed={filters.tags.includes(tag.value)}
						onclick={() => onchange(toggleTag(filters, tag.value))}
					>
						{tag.value}
						{#if tag.count}<Text tone="faint" size="xs" numeric>{tag.count}</Text>{/if}
					</Toggle>
				{/each}
			</div>
		</Stack>
	{/if}
</Stack>

<style>
	.pair {
		display: grid;
		grid-template-columns: repeat(2, minmax(0, 1fr));
		gap: var(--sp-2);
	}
	.amount {
		display: grid;
		grid-template-columns: minmax(0, 1fr) minmax(0, 1fr) minmax(5rem, auto);
		gap: var(--sp-2);
	}
	.tags {
		display: flex;
		flex-wrap: wrap;
		gap: var(--sp-1);
	}
</style>
