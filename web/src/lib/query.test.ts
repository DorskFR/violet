import { describe, expect, it } from 'vitest';
import type { Understood } from './api';
import {
	activeFilters,
	EMPTY_FILTERS,
	type Filters,
	filtersHref,
	filtersToParams,
	literal,
	parseFilters,
	toggleTag,
	toSearchParams,
	understoodChips,
	withYear,
	yearOf,
} from './query';

const filters = (over: Partial<Filters>): Filters => ({ ...EMPTY_FILTERS, ...over });

describe('parseFilters', () => {
	it('is empty for an empty query string', () => {
		expect(parseFilters(new URLSearchParams(''))).toEqual(EMPTY_FILTERS);
	});

	it('reads every parameter', () => {
		const params = new URLSearchParams(
			'q=rent&area=Housing&type=receipt&issuer=UR&tag=2024&tag=tax&from=2024-01-01&to=2024-06-30&min=1000&max=5000&currency=jpy',
		);
		expect(parseFilters(params)).toEqual({
			q: 'rent',
			area: 'Housing',
			type: 'receipt',
			issuer: 'UR',
			tags: ['2024', 'tax'],
			from: '2024-01-01',
			to: '2024-06-30',
			min: '1000',
			max: '5000',
			currency: 'JPY',
		});
	});

	it('drops malformed dates, blank and repeated tags, unknown keys', () => {
		const params = new URLSearchParams('from=yesterday&to=2024-02-31&tag=&tag=a&tag=a&x=1');
		expect(parseFilters(params)).toEqual(filters({ tags: ['a'] }));
	});
});

describe('filtersToParams', () => {
	it('writes nothing for empty filters', () => {
		expect(filtersToParams(EMPTY_FILTERS).toString()).toBe('');
		expect(filtersHref(EMPTY_FILTERS)).toBe('/');
	});

	it('writes a stable key order', () => {
		const f = filters({ currency: 'JPY', min: '100', tags: ['b', 'a'], q: 'x', area: 'Tax' });
		expect(filtersToParams(f).toString()).toBe('q=x&area=Tax&tag=b&tag=a&min=100&currency=JPY');
	});

	it('round-trips, Japanese included', () => {
		const f = filters({
			q: '領収書 "東京 ガス"',
			issuer: '東京ガス',
			tags: ['医療費', 'tax & co'],
			from: '2023-04-01',
			max: '12.50',
			currency: 'USD',
		});
		expect(parseFilters(filtersToParams(f))).toEqual(f);
		expect(parseFilters(new URL(filtersHref(f), 'http://x').searchParams)).toEqual(f);
	});

	it('trims the text', () => {
		expect(filtersToParams(filters({ q: '  gas  ' })).get('q')).toBe('gas');
		expect(filtersToParams(filters({ q: '   ' })).has('q')).toBe(false);
	});
});

describe('toSearchParams', () => {
	it('adds paging and leaves out what is unset', () => {
		expect(toSearchParams(EMPTY_FILTERS, 30, 60)).toEqual({
			q: undefined,
			area: undefined,
			type: undefined,
			issuer: undefined,
			tag: undefined,
			from: undefined,
			to: undefined,
			min: undefined,
			max: undefined,
			currency: undefined,
			limit: 30,
			offset: 60,
		});
	});

	it('converts amounts to minor units of the currency', () => {
		const usd = toSearchParams(filters({ min: '12.50', max: '100', currency: 'USD' }), 30, 0);
		expect([usd.min, usd.max, usd.currency]).toEqual([1250, 10000, 'USD']);
		const jpy = toSearchParams(filters({ min: '1,200', currency: 'JPY' }), 30, 0);
		expect([jpy.min, jpy.max, jpy.currency]).toEqual([1200, undefined, 'JPY']);
	});

	it('sends no currency without an amount, and skips amounts that are not numbers', () => {
		const p = toSearchParams(filters({ min: 'lots', currency: 'JPY' }), 30, 0);
		expect([p.min, p.currency]).toEqual([undefined, undefined]);
	});
});

describe('year', () => {
	it('maps a year to a full-year range and back', () => {
		const f = withYear(EMPTY_FILTERS, '2024');
		expect([f.from, f.to]).toEqual(['2024-01-01', '2024-12-31']);
		expect(yearOf(f)).toBe('2024');
		expect(withYear(f, '')).toEqual(EMPTY_FILTERS);
	});

	it('is not a year for a partial range', () => {
		expect(yearOf({ from: '2024-01-01', to: '2024-06-30' })).toBe('');
		expect(yearOf({ from: '2023-01-01', to: '2024-12-31' })).toBe('');
		expect(yearOf({ from: '', to: '' })).toBe('');
	});
});

describe('active filters', () => {
	it('toggles tags', () => {
		expect(toggleTag(filters({ tags: ['a'] }), 'b').tags).toEqual(['a', 'b']);
		expect(toggleTag(filters({ tags: ['a', 'b'] }), 'a').tags).toEqual(['b']);
	});

	it('lists each filter with the state that removes it', () => {
		const f = filters({
			q: 'x',
			area: 'Tax',
			tags: ['a'],
			from: '2024-01-01',
			to: '2024-12-31',
			min: '100',
			currency: 'JPY',
		});
		const active = activeFilters(f);
		expect(active.map((a) => a.label)).toEqual(['Tax', '#a', '2024', '≥ 100 JPY']);
		expect(active[0]?.cleared).toEqual({ ...f, area: '' });
		expect(active[2]?.cleared).toEqual({ ...f, from: '', to: '' });
		expect(active[3]?.cleared).toEqual({ ...f, min: '', currency: '' });
	});

	it('shows open-ended dates separately', () => {
		expect(activeFilters(filters({ from: '2024-03-01' })).map((a) => a.label)).toEqual([
			'from 2024-03-01',
		]);
	});
});

describe('literal', () => {
	it('quotes the first free occurrence of the text', () => {
		expect(literal('facture EDF 2023', '2023')).toBe('facture EDF "2023"');
		expect(literal('133,90  € facture', '133,90 €')).toBe('"133,90 €" facture');
		expect(literal('"2023" x 2023', '2023')).toBe('"2023" x "2023"');
		expect(literal('a2023 2023', '2023')).toBe('a2023 "2023"');
		expect(literal('2024年3月分電気', '2024年3月分')).toBe('"2024年3月分" 電気');
	});

	it('appends the text when it is not in q', () => {
		expect(literal('facture', 'mars')).toBe('facture "mars"');
		expect(literal('', 'mars')).toBe('"mars"');
	});
});

describe('understoodChips', () => {
	it('is empty without understood filters', () => {
		expect(understoodChips(undefined, 'x')).toEqual([]);
		expect(understoodChips({ terms: [] }, 'x')).toEqual([]);
	});

	it('labels each kind and quotes its words when removed', () => {
		const chips = understoodChips(
			{
				from: '2023-01-01',
				to: '2023-12-31',
				min: 13390,
				max: 13390,
				currency: 'EUR',
				latest: true,
				terms: [
					{ text: 'dernier', kind: 'latest' },
					{ text: '133,90 €', kind: 'amount' },
					{ text: '2023', kind: 'date' },
				],
			},
			'dernier facture 133,90 € 2023',
			'en',
		);
		expect(chips).toEqual([
			{ key: 'understood:date', label: '2023', q: 'dernier facture 133,90 € "2023"' },
			{ key: 'understood:amount', label: '€133.90', q: 'dernier facture "133,90 €" 2023' },
			{ key: 'understood:latest', label: 'Newest first', q: '"dernier" facture 133,90 € 2023' },
		]);
	});

	it('names months and ranges', () => {
		const label = (u: Omit<Understood, 'terms'>) =>
			understoodChips({ ...u, terms: [{ text: 'x', kind: 'date' }] }, 'x', 'en')[0]?.label;
		expect(label({ from: '2014-03-01', to: '2014-03-31' })).toBe('March 2014');
		expect(label({ from: '2022-01-01', to: '2023-12-31' })).toBe('2022–2023');
		expect(label({ from: '2022-01-01', to: '2023-12-31', month: 3 })).toBe('March, 2022–2023');
		expect(label({ month: 3 })).toBe('March, any year');
		expect(label({ from: '2024-01-01', to: '2024-01-31' })).toBe('January 2024');
	});

	it('shows amounts without a currency in hundredths and ranges', () => {
		const label = (u: Omit<Understood, 'terms'>) =>
			understoodChips({ ...u, terms: [{ text: 'x', kind: 'amount' }] }, 'x', 'en')[0]?.label;
		expect(label({ min: 13390, max: 13390 })).toBe('133.90');
		expect(label({ min: 3000, max: 5000, currency: 'JPY' })).toBe('¥3,000–¥5,000');
	});
});
