import { describe, expect, it } from 'vitest';
import type { Sidecar } from './api';
import { draftOf, factLabel, patchOf } from './metadata';

const sidecar: Sidecar = {
	version: 1,
	id: 'a',
	title: 'Clinic receipt',
	area: 'Health',
	doc_type: 'receipt',
	issuer: 'Sakura Clinic',
	date: '2026-11-04',
	language: 'ja',
	summary: null,
	tags: ['医療費', 'tax'],
	facts: [
		{ key: 'total', value: '2,340円', amount_minor: 2340, currency: 'JPY' },
		{ key: 'fee', value: '$12.50', amount_minor: 1250, currency: 'USD' },
		{ key: 'points', value: '780点', amount_minor: null, currency: null },
	],
	pages: [],
	sources: [],
	sha256: 'x',
	confidence: 0.9,
	created_at: '2026-11-04T00:00:00Z',
	updated_at: '2026-11-04T00:00:00Z',
};

describe('draft', () => {
	it('fills the form from the sidecar', () => {
		const draft = draftOf(sidecar);
		expect(draft.tags).toBe('医療費, tax');
		expect(draft.facts.map((f) => f.amount)).toEqual(['2340', '12.50', '']);
	});

	it('produces an empty patch when nothing changed', () => {
		expect(patchOf(sidecar, draftOf(sidecar))).toEqual({ patch: {}, errors: null });
	});

	it('sends only what changed', () => {
		const draft = { ...draftOf(sidecar), title: ' Clinic receipt Nov ', issuer: ' ', date: '' };
		expect(patchOf(sidecar, draft).patch).toEqual({
			title: 'Clinic receipt Nov',
			issuer: null,
			date: null,
		});
	});

	it('sends tags and facts as whole lists', () => {
		const draft = draftOf(sidecar);
		draft.tags = '医療費, tax, 2026';
		draft.facts = [
			{ key: 'total', value: '', amount: '2,500', currency: 'jpy' },
			{ key: '', value: '', amount: '', currency: '' },
		];
		expect(patchOf(sidecar, draft).patch).toEqual({
			tags: ['医療費', 'tax', '2026'],
			facts: [{ key: 'total', value: '2,500', amount_minor: 2500, currency: 'JPY' }],
		});
	});

	it('explains what blocks a save', () => {
		const draft = draftOf(sidecar);
		draft.title = ' ';
		draft.area = 'Health/2024';
		draft.date = '2026-02-30';
		draft.facts = [
			{ key: '', value: 'x', amount: '', currency: '' },
			{ key: 'total', value: '', amount: 'lots', currency: 'JPY' },
			{ key: 'total', value: '', amount: '12', currency: '' },
		];
		const { patch, errors } = patchOf(sidecar, draft);
		expect(patch).toBeNull();
		expect(Object.keys(errors ?? {}).sort()).toEqual(['area', 'date', 'facts', 'title']);
		expect(Object.keys(errors?.facts ?? {})).toEqual(['0', '1', '2']);
	});

	it('labels fact keys', () => {
		expect(factLabel('net_pay')).toBe('Net pay');
		expect(factLabel('total')).toBe('Total');
	});
});
