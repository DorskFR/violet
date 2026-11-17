import { afterEach, describe, expect, it, vi } from 'vitest';
import { ApiError, api, fileUrl, pageSrcset, pageUrl, searchQuery } from './api';

function mockFetch(response: Response | Error) {
	const fn = vi.fn(async (_url: string, _init?: RequestInit) => {
		if (response instanceof Error) throw response;
		return response;
	});
	vi.stubGlobal('fetch', fn);
	return fn;
}

const json = (body: unknown, status = 200) =>
	new Response(JSON.stringify(body), { status, headers: { 'content-type': 'application/json' } });

afterEach(() => vi.unstubAllGlobals());

describe('searchQuery', () => {
	it('leaves out empty values and repeats tags', () => {
		expect(
			searchQuery({ q: '  rent ', area: 'Housing', type: '', tag: ['2024', 'tax'], limit: 30 }),
		).toBe('q=rent&area=Housing&tag=2024&tag=tax&limit=30');
	});

	it('keeps zero amounts and offsets', () => {
		expect(searchQuery({ min: 0, max: 5000, currency: 'JPY', offset: 0 })).toBe(
			'min=0&max=5000&currency=JPY&offset=0',
		);
	});

	it('encodes Japanese text', () => {
		expect(
			new URLSearchParams(searchQuery({ q: '領収書', issuer: '東京ガス' })).get('issuer'),
		).toBe('東京ガス');
	});
});

describe('api', () => {
	it('searches with a query string', async () => {
		const fetch = mockFetch(json({ total: 0, hits: [] }));
		await expect(api.search({ q: 'gas', limit: 10 })).resolves.toEqual({ total: 0, hits: [] });
		expect(fetch.mock.calls[0]?.[0]).toBe('/api/search?q=gas&limit=10');
		expect(fetch.mock.calls[0]?.[1]?.method).toBe('GET');
	});

	it('searches without parameters', async () => {
		const fetch = mockFetch(json({ total: 0, hits: [] }));
		await api.search();
		expect(fetch.mock.calls[0]?.[0]).toBe('/api/search');
	});

	it('patches a document with a JSON body', async () => {
		const fetch = mockFetch(json({ path: 'a.pdf', size: 1, sidecar: {} }));
		await api.update('abc', { title: 'New', tags: ['x'] });
		const [url, init] = fetch.mock.calls[0] ?? [];
		expect(url).toBe('/api/documents/abc');
		expect(init?.method).toBe('PATCH');
		expect(new Headers(init?.headers).get('content-type')).toBe('application/json');
		expect(JSON.parse(String(init?.body))).toEqual({ title: 'New', tags: ['x'] });
	});

	it('sends split groups, rotations and page deletions', async () => {
		const fetch = mockFetch(json({ documents: ['a', 'b'] }));
		await api.split('abc', [[1, 2], [3]]);
		expect(fetch.mock.calls[0]?.[0]).toBe('/api/documents/abc/split');
		expect(JSON.parse(String(fetch.mock.calls[0]?.[1]?.body))).toEqual({ groups: [[1, 2], [3]] });

		const rotate = mockFetch(json({}));
		await api.rotate('abc', [2], 90);
		expect(rotate.mock.calls[0]?.[0]).toBe('/api/documents/abc/rotate');
		expect(JSON.parse(String(rotate.mock.calls[0]?.[1]?.body))).toEqual({
			pages: [2],
			degrees: 90,
		});

		const del = mockFetch(json({}));
		await api.deletePages('abc', [2, 3]);
		expect(del.mock.calls[0]?.[0]).toBe('/api/documents/abc/delete-pages');
		expect(JSON.parse(String(del.mock.calls[0]?.[1]?.body))).toEqual({ pages: [2, 3] });
	});

	it('resolves a 204 to undefined', async () => {
		const fetch = mockFetch(new Response(null, { status: 204 }));
		await expect(api.remove('abc')).resolves.toBeUndefined();
		expect(fetch.mock.calls[0]?.[1]?.method).toBe('DELETE');
	});

	it('merges by id list', async () => {
		const fetch = mockFetch(json({}));
		await api.merge(['a', 'b']);
		expect(fetch.mock.calls[0]?.[0]).toBe('/api/merge');
		expect(JSON.parse(String(fetch.mock.calls[0]?.[1]?.body))).toEqual({ ids: ['a', 'b'] });
	});

	it('uploads every file under the field "file" and lets the browser set the boundary', async () => {
		const fetch = mockFetch(json({ queued: 2 }));
		const files = [new File(['a'], 'a.pdf'), new File(['b'], '領収書.jpg')];
		await expect(api.upload(files)).resolves.toEqual({ queued: 2 });
		const init = fetch.mock.calls[0]?.[1];
		const body = init?.body as FormData;
		expect(body.getAll('file').map((f) => (f as File).name)).toEqual(['a.pdf', '領収書.jpg']);
		expect(new Headers(init?.headers).has('content-type')).toBe(false);
	});

	it('posts an undo', async () => {
		const fetch = mockFetch(json({ entry: { id: 'u' } }));
		await api.undo('e1');
		expect(fetch.mock.calls[0]?.[0]).toBe('/api/activity/e1/undo');
		expect(fetch.mock.calls[0]?.[1]?.method).toBe('POST');
	});

	it('turns {"error"} bodies into ApiError', async () => {
		mockFetch(json({ error: 'no such document' }, 404));
		const err = await api.document('nope').catch((e: unknown) => e);
		expect(err).toBeInstanceOf(ApiError);
		expect((err as ApiError).status).toBe(404);
		expect((err as ApiError).message).toBe('no such document');
	});

	it('falls back to the status text for non-JSON errors', async () => {
		mockFetch(new Response('<html>bad gateway</html>', { status: 502, statusText: 'Bad Gateway' }));
		const err = (await api.status().catch((e: unknown) => e)) as ApiError;
		expect(err.status).toBe(502);
		expect(err.message).toBe('Bad Gateway');
	});

	it('reports an unreachable server in plain words', async () => {
		mockFetch(new TypeError('fetch failed'));
		const err = (await api.facets().catch((e: unknown) => e)) as ApiError;
		expect(err.status).toBe(0);
		expect(err.message).toBe('The server could not be reached.');
	});
});

describe('urls', () => {
	it('builds page image urls with a width and a cache key', () => {
		expect(pageUrl('abc', 2, 600)).toBe('/api/documents/abc/pages/2?w=600');
		expect(pageUrl('abc', 2, 600, 'deadbeef')).toBe('/api/documents/abc/pages/2?w=600&v=deadbeef');
	});

	it('builds a srcset with width descriptors', () => {
		expect(pageSrcset('abc', 1, [600, 1200])).toBe(
			'/api/documents/abc/pages/1?w=600 600w, /api/documents/abc/pages/1?w=1200 1200w',
		);
	});

	it('builds the file url', () => {
		expect(fileUrl('a b')).toBe('/api/documents/a%20b/file');
	});
});
