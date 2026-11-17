import { describe, expect, it } from 'vitest';
import { affectedDocuments } from './activity';

describe('affectedDocuments', () => {
	it('links the documents an ingest produced', () => {
		expect(
			affectedDocuments({
				kind: 'ingest',
				original: 'o.pdf',
				original_name: 'scan.pdf',
				documents: [
					['a', 'Health/2024/a.pdf'],
					['b', 'Tax/2024/b.pdf'],
				],
			}),
		).toEqual([
			{ id: 'a', label: 'a.pdf', gone: false },
			{ id: 'b', label: 'b.pdf', gone: false },
		]);
	});

	it('marks split and merge inputs as gone', () => {
		expect(
			affectedDocuments({
				kind: 'split',
				original: 'o',
				from: ['a', 'x/a.pdf'],
				into: [['b', 'x/b.pdf']],
			}).map((d) => [d.id, d.gone]),
		).toEqual([
			['a', true],
			['b', false],
		]);
		expect(
			affectedDocuments({
				kind: 'merge',
				originals: [
					['o1', 'a', 'x/a.pdf'],
					['o2', 'b', 'x/b.pdf'],
				],
				into: ['c', 'x/c.pdf'],
			}).map((d) => [d.id, d.gone]),
		).toEqual([
			['a', true],
			['b', true],
			['c', false],
		]);
	});

	it('uses the new path after an update and nothing for an undo', () => {
		const before = {} as never;
		expect(
			affectedDocuments({
				kind: 'update',
				document: 'a',
				from: 'x/old.pdf',
				to: 'y/new.pdf',
				before,
			}),
		).toEqual([{ id: 'a', label: 'new.pdf', gone: false }]);
		expect(affectedDocuments({ kind: 'undo', entry: 'e' })).toEqual([]);
	});
});
