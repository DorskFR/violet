import { describe, expect, it } from 'vitest';
import { parseSnippet } from '../src/lib/snippet.ts';
import { pagePng } from './png.ts';
import { MockStore } from './store.ts';

const q = (query: string) => new URLSearchParams(query);

describe('mock backend', () => {
	it('serves thirty documents, newest first', () => {
		const result = new MockStore().search(q('limit=100'));
		expect(result.total).toBe(30);
		const dates = result.hits.map((h) => h.date ?? '');
		expect(dates).toEqual([...dates].sort().reverse());
	});

	it('filters, pages and escapes snippets', () => {
		const store = new MockStore();
		expect(store.search(q('area=Health')).hits.every((h) => h.area === 'Health')).toBe(true);
		expect(store.search(q('limit=7&offset=28')).hits).toHaveLength(2);
		expect(store.search(q('min=100000&max=200000&currency=JPY')).total).toBeGreaterThan(0);
		const hit = store.search(q('q=taxpayer')).hits[0];
		expect(hit?.snippet).toContain('<mark>Taxpayer</mark>');
		expect(hit?.snippet).toContain('&lt;b&gt;');
		expect(parseSnippet(hit?.snippet).some((p) => p.mark)).toBe(true);
	});

	it('undoes a delete and a split through the journal', () => {
		const store = new MockStore();
		const doc = store.search(q('q=Employment')).hits[0];
		if (!doc) throw new Error('fixture missing');
		const { documents } = store.split(doc.id, [
			[1, 2],
			[3, 4, 5],
		]);
		expect(documents).toHaveLength(2);
		expect(() => store.document(doc.id)).toThrow('no such document');
		const entry = store.activity(1)[0];
		expect(entry?.entry.action.kind).toBe('split');
		expect(entry?.undoable).toBe(true);
		store.undo(entry?.entry.id ?? '');
		expect(store.document(doc.id).sidecar.pages).toHaveLength(5);
		const feed = store.activity(2);
		expect(feed[0]?.entry.action.kind).toBe('undo');
		expect(feed[1]?.undone).toBe(true);
		expect(feed[1]?.undoable).toBe(false);
	});

	it('answers the journal entry of every change, and none for a patch that changes nothing', () => {
		const store = new MockStore();
		const [a, b, c] = store.search(q('limit=3')).hits.map((h) => h.id);
		if (!a || !b || !c) throw new Error('fixture missing');
		const latest = () => store.activity(1)[0]?.entry.id;

		const saved = store.update(a, { title: 'Renamed', issuer: null });
		expect(saved.entry).toBe(latest());
		expect(saved.sidecar.issuer).toBeNull();
		expect(store.update(a, { title: 'Renamed' }).entry).toBeNull();
		expect(store.rotate(a, [1], 90).entry).toBe(latest());

		const reread = store.reanalyze(a);
		expect(reread.entry).toBe(latest());
		expect(reread.sidecar.confidence).toBe(0.9);
		store.undo(reread.entry ?? '');
		expect(store.document(a).sidecar.confidence).toBe(1);

		const removed = store.remove(b);
		expect(removed.entry).toBe(latest());
		store.undo(removed.entry);
		expect(store.document(b).sidecar.id).toBe(b);

		const merged = store.merge([b, c]);
		expect(merged.entry).toBe(latest());
		store.undo(merged.entry ?? '');
		expect(store.document(c).sidecar.id).toBe(c);
		expect(() => store.document(merged.sidecar.id)).toThrow('no such document');
	});

	it('counts documents by the currency of their facts', () => {
		const { currencies } = new MockStore().facets();
		expect(currencies[0]?.value).toBe('JPY');
		expect(currencies.every((c) => c.count > 0)).toBe(true);
		const counts = currencies.map((c) => c.count);
		expect(counts).toEqual([...counts].sort((x, y) => y - x));
	});

	it('renders a PNG that turns with the page', () => {
		const dims = (png: Buffer) => [png.readUInt32BE(16), png.readUInt32BE(20)];
		expect(pagePng(1, 400, 0).subarray(1, 4).toString('latin1')).toBe('PNG');
		expect(dims(pagePng(1, 400, 0))).toEqual([283, 400]);
		expect(dims(pagePng(1, 400, 90))).toEqual([400, 283]);
	});
});
