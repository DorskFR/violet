import { describe, expect, it } from 'vitest';
import type { DuplicatePair, Hit } from './api';
import { documentCount, hasPages, pairKey, withoutDocument, withoutPair } from './duplicates';

const hit = (id: string, path = `Housing/2026/${id}.pdf`): Hit => ({
	id,
	path,
	title: id,
	area: 'Housing',
	doc_type: 'invoice',
	issuer: 'SCI Malical',
	date: '2026-07-01',
	tags: [],
	amount_minor: 140000,
	currency: 'EUR',
	pages: 1,
	snippet: null,
});

const pair = (a: string, b: string): DuplicatePair => [hit(a), hit(b)];

describe('duplicates', () => {
	const pairs = [pair('a', 'b'), pair('a', 'c'), pair('d', 'e')];

	it('drops a dismissed pair in either order and keeps the others', () => {
		expect(withoutPair(pairs, 'b', 'a').map(pairKey)).toEqual(['a:c', 'd:e']);
		expect(withoutPair(pairs, 'a', 'b').map(pairKey)).toEqual(['a:c', 'd:e']);
		expect(withoutPair(pairs, 'b', 'c')).toEqual(pairs);
	});

	it('drops every pair of a deleted document', () => {
		expect(withoutDocument(pairs, 'a').map(pairKey)).toEqual(['d:e']);
		expect(withoutDocument(pairs, 'e').map(pairKey)).toEqual(['a:b', 'a:c']);
		expect(withoutDocument(pairs, 'z')).toEqual(pairs);
	});

	it('counts each document once', () => {
		expect(documentCount(pairs)).toBe(5);
		expect(documentCount([])).toBe(0);
	});

	it('shows pages only for PDFs', () => {
		expect(hasPages(hit('a', 'Work/a.PDF'))).toBe(true);
		expect(hasPages(hit('a', 'Work/a.docx'))).toBe(false);
	});
});
