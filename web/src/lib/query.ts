import type { SearchParams, Understood, UnderstoodTerm } from './api';
import { formatAmount, isIsoDate, parseAmount } from './format';

/** The search screen's state, as kept in the address bar. Amounts are major units as typed. */
export type Filters = {
	q: string;
	area: string;
	type: string;
	issuer: string;
	tags: string[];
	from: string;
	to: string;
	min: string;
	max: string;
	currency: string;
};

export const EMPTY_FILTERS: Filters = {
	q: '',
	area: '',
	type: '',
	issuer: '',
	tags: [],
	from: '',
	to: '',
	min: '',
	max: '',
	currency: '',
};

const SINGLE = ['q', 'area', 'type', 'issuer', 'from', 'to', 'min', 'max', 'currency'] as const;

export function parseFilters(params: Pick<URLSearchParams, 'get' | 'getAll'>): Filters {
	const get = (key: string) => params.get(key)?.trim() ?? '';
	const date = (key: string) => (isIsoDate(get(key)) ? get(key) : '');
	const tags = [...new Set(params.getAll('tag').map((t) => t.trim()))].filter(Boolean);
	return {
		q: get('q'),
		area: get('area'),
		type: get('type'),
		issuer: get('issuer'),
		tags,
		from: date('from'),
		to: date('to'),
		min: get('min'),
		max: get('max'),
		currency: get('currency').toUpperCase(),
	};
}

/** Canonical query string: fixed key order, empty values left out. */
export function filtersToParams(filters: Filters): URLSearchParams {
	const out = new URLSearchParams();
	for (const key of SINGLE) {
		const value = filters[key].trim();
		if (value) out.set(key, value);
		if (key === 'issuer') for (const tag of filters.tags) out.append('tag', tag);
	}
	return out;
}

export function filtersHref(filters: Filters, pathname = '/'): string {
	const query = filtersToParams(filters).toString();
	return query ? `${pathname}?${query}` : pathname;
}

export function toSearchParams(filters: Filters, limit: number, offset: number): SearchParams {
	const currency = filters.currency || undefined;
	const min = parseAmount(filters.min, currency);
	const max = parseAmount(filters.max, currency);
	return {
		q: filters.q.trim() || undefined,
		area: filters.area || undefined,
		type: filters.type || undefined,
		issuer: filters.issuer || undefined,
		tag: filters.tags.length ? filters.tags : undefined,
		from: filters.from || undefined,
		to: filters.to || undefined,
		min: min ?? undefined,
		max: max ?? undefined,
		currency: min !== null || max !== null ? currency : undefined,
		limit,
		offset,
	};
}

/** The year when the date range is exactly one calendar year, else ''. */
export function yearOf(filters: Pick<Filters, 'from' | 'to'>): string {
	const year = filters.from.slice(0, 4);
	return filters.from === `${year}-01-01` && filters.to === `${year}-12-31` ? year : '';
}

export function withYear(filters: Filters, year: string): Filters {
	return /^\d{4}$/.test(year)
		? { ...filters, from: `${year}-01-01`, to: `${year}-12-31` }
		: { ...filters, from: '', to: '' };
}

export function toggleTag(filters: Filters, tag: string): Filters {
	const tags = filters.tags.includes(tag)
		? filters.tags.filter((t) => t !== tag)
		: [...filters.tags, tag];
	return { ...filters, tags };
}

export type ActiveFilter = { key: string; label: string; cleared: Filters };

/** Every filter in effect besides the text, each with the state that results from removing it. */
export function activeFilters(filters: Filters): ActiveFilter[] {
	const out: ActiveFilter[] = [];
	if (filters.area)
		out.push({ key: 'area', label: filters.area, cleared: { ...filters, area: '' } });
	if (filters.type)
		out.push({ key: 'type', label: filters.type, cleared: { ...filters, type: '' } });
	if (filters.issuer) {
		out.push({ key: 'issuer', label: filters.issuer, cleared: { ...filters, issuer: '' } });
	}
	for (const tag of filters.tags) {
		out.push({ key: `tag:${tag}`, label: `#${tag}`, cleared: toggleTag(filters, tag) });
	}
	const year = yearOf(filters);
	if (year) {
		out.push({ key: 'year', label: year, cleared: withYear(filters, '') });
	} else {
		if (filters.from) {
			out.push({ key: 'from', label: `from ${filters.from}`, cleared: { ...filters, from: '' } });
		}
		if (filters.to) {
			out.push({ key: 'to', label: `until ${filters.to}`, cleared: { ...filters, to: '' } });
		}
	}
	if (filters.min || filters.max) {
		const unit = filters.currency ? ` ${filters.currency}` : '';
		const label =
			filters.min && filters.max
				? `${filters.min}–${filters.max}${unit}`
				: filters.min
					? `≥ ${filters.min}${unit}`
					: `≤ ${filters.max}${unit}`;
		out.push({ key: 'amount', label, cleared: { ...filters, min: '', max: '', currency: '' } });
	}
	return out;
}

/** `q` with `text` in quotes, so the server reads it as words again. */
export function literal(q: string, text: string): string {
	const words = q.trim().split(/\s+/).join(' ');
	let from = 0;
	for (;;) {
		const at = words.indexOf(text, from);
		if (at < 0) return words ? `${words} "${text}"` : `"${text}"`;
		const quotes = (words.slice(0, at).match(/"/g) ?? []).length;
		if ((at === 0 || words[at - 1] === ' ') && quotes % 2 === 0) {
			const after = words.slice(at + text.length);
			const gap = after && !after.startsWith(' ') ? ' ' : '';
			return `${words.slice(0, at)}"${text}"${gap}${after}`;
		}
		from = at + 1;
	}
}

export type UnderstoodChip = { key: string; label: string; q: string };

function monthName(month: number, locale?: string): string {
	return new Intl.DateTimeFormat(locale, { month: 'long', timeZone: 'UTC' }).format(
		Date.UTC(2000, month - 1, 1),
	);
}

/** `2023`, `2022–2023`, `March 2024`, `March, 2022–2023` or `March, any year`. */
function dateLabel(understood: Understood, locale?: string): string {
	const { from, to, month } = understood;
	const name = month ? monthName(month, locale) : '';
	if (!from || !to) return name ? `${name}, any year` : '';
	const [year, last] = [from.slice(0, 4), to.slice(0, 4)];
	if (!from.endsWith('-01-01') || !to.endsWith('-12-31')) {
		return `${monthName(Number(from.slice(5, 7)), locale)} ${year}`;
	}
	const years = year === last ? year : `${year}–${last}`;
	return name ? `${name}, ${years}` : years;
}

function amountLabel(understood: Understood, locale?: string): string {
	const one = (minor: number) =>
		understood.currency
			? formatAmount(minor, understood.currency, locale)
			: new Intl.NumberFormat(locale, { minimumFractionDigits: 2 }).format(minor / 100);
	const { min, max } = understood;
	if (min === undefined || max === undefined) return '';
	return min === max ? one(min) : `${one(min)}–${one(max)}`;
}

/**
 * One chip per kind of filter the server read from `q`; removing it quotes the words it came
 * from, so they are searched as text instead.
 */
export function understoodChips(
	understood: Understood | undefined,
	q: string,
	locale?: string,
): UnderstoodChip[] {
	if (!understood) return [];
	const chips: UnderstoodChip[] = [];
	const kinds: [UnderstoodTerm['kind'], string][] = [
		['date', dateLabel(understood, locale)],
		['amount', amountLabel(understood, locale)],
		['latest', understood.latest ? 'Newest first' : ''],
	];
	for (const [kind, label] of kinds) {
		const terms = understood.terms.filter((term) => term.kind === kind);
		if (!terms.length || !label) continue;
		const quoted = terms.reduce((text, term) => literal(text, term.text), q);
		chips.push({ key: `understood:${kind}`, label, q: quoted });
	}
	return chips;
}
