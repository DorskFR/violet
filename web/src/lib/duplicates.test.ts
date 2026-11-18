import { describe, expect, it } from 'vitest';
import type { DuplicateCopy, DuplicatePair } from './api';
import {
	differences,
	documentCount,
	hasPages,
	origins,
	pairKey,
	sameFile,
	withoutDocument,
	withoutPair,
} from './duplicates';

const hit = (id: string, path = `Housing/2026/${id}.pdf`): DuplicateCopy => ({
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
	created_at: '2026-07-02T09:00:00Z',
	size: 48_000,
	sha256: `sha-${id}`,
	sources: [{ original_name: `${id}.pdf` }],
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

	it('names the fields that differ between the two sides', () => {
		const a = hit('a');
		expect(differences(a, { ...a, id: 'b' })).toEqual([]);
		const b = { ...hit('b'), title: 'a', pages: 2, size: 50_000, issuer: 'SOCIETE GENERALE' };
		expect(differences(a, b)).toEqual(['issuer', 'pages', 'size']);
		expect(differences(a, { ...a, date: null, area: 'Bank' })).toEqual(['area', 'date']);
	});

	it('tells the same file apart from a lookalike', () => {
		const [a, b] = pair('a', 'b');
		expect(sameFile([a, b])).toBe(false);
		expect(sameFile([a, { ...b, sha256: a.sha256 }])).toBe(true);
	});

	it('says where a copy came from: the mail, else the file name', () => {
		expect(origins(hit('a'))).toEqual(['a.pdf']);
		const mail = { account: 'home', subject: 'Votre relevé', date: null };
		expect(
			origins({
				sources: [
					{ original_name: 'releve.pdf', mail },
					{ original_name: 'releve.pdf', mail },
					{ original_name: 'scan.pdf', mail: { ...mail, subject: ' ' } },
					{ original_name: 'export.pdf' },
				],
			}),
		).toEqual(['Mail to home: Votre relevé', 'Mail to home: no subject', 'export.pdf']);
	});
});
