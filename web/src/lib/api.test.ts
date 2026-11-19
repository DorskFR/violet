import { afterEach, describe, expect, it, vi } from 'vitest';
import {
	ApiError,
	api,
	fileUrl,
	type Me,
	onUnauthorized,
	pageSrcset,
	pageUrl,
	rememberCsrf,
	searchQuery,
} from './api';

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

afterEach(() => {
	vi.unstubAllGlobals();
	rememberCsrf(null);
	onUnauthorized(() => {});
});

const me = (over: Partial<Me> = {}): Me => ({
	csrf: 'csrf-1',
	name: 'alex',
	role: 'admin',
	scope: 'admin',
	via: 'session',
	areas: [],
	users: true,
	password: true,
	passkeys: [],
	...over,
});

const header = (init: RequestInit | undefined, name: string) =>
	new Headers(init?.headers).get(name);

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
		const fetch = mockFetch(json({ entry: 'e1', path: 'a.pdf', size: 1, sidecar: {} }));
		const changed = await api.update('abc', { title: 'New', issuer: null, tags: ['x'] });
		expect(changed.entry).toBe('e1');
		const [url, init] = fetch.mock.calls[0] ?? [];
		expect(url).toBe('/api/documents/abc');
		expect(init?.method).toBe('PATCH');
		expect(new Headers(init?.headers).get('content-type')).toBe('application/json');
		expect(JSON.parse(String(init?.body))).toEqual({ title: 'New', issuer: null, tags: ['x'] });
	});

	it('sends split groups, rotations and page deletions', async () => {
		const fetch = mockFetch(json({ entry: 'e1', documents: ['a', 'b'] }));
		await expect(api.split('abc', [[1, 2], [3]])).resolves.toEqual({
			entry: 'e1',
			documents: ['a', 'b'],
		});
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

	it('asks for a document to be read again', async () => {
		const fetch = mockFetch(json({ entry: 'e1', path: 'a.pdf', size: 1, sidecar: {} }));
		expect((await api.reanalyze('abc')).entry).toBe('e1');
		expect(fetch.mock.calls[0]?.[0]).toBe('/api/documents/abc/reanalyze');
		expect(fetch.mock.calls[0]?.[1]?.method).toBe('POST');
	});

	it('deletes a document and answers the journal entry', async () => {
		const fetch = mockFetch(json({ entry: 'e1' }));
		await expect(api.remove('abc')).resolves.toEqual({ entry: 'e1' });
		expect(fetch.mock.calls[0]?.[0]).toBe('/api/documents/abc');
		expect(fetch.mock.calls[0]?.[1]?.method).toBe('DELETE');
	});

	it('lists the possible duplicates', async () => {
		const fetch = mockFetch(json({ pairs: [] }));
		await expect(api.duplicates()).resolves.toEqual({ pairs: [] });
		expect(fetch.mock.calls[0]?.[0]).toBe('/api/duplicates');
		expect(fetch.mock.calls[0]?.[1]?.method).toBe('GET');
	});

	it('says two documents are not duplicates as JSON', async () => {
		const fetch = mockFetch(
			json({ entry: 'e1', path: 'a.pdf', size: 1, sidecar: {}, duplicates: [] }),
		);
		expect((await api.notDuplicate('a b', 'c')).entry).toBe('e1');
		expect(fetch.mock.calls[0]?.[0]).toBe('/api/documents/a%20b/not-duplicate');
		const init = fetch.mock.calls[0]?.[1];
		expect(init?.method).toBe('POST');
		expect(new Headers(init?.headers).get('content-type')).toBe('application/json');
		expect(JSON.parse(String(init?.body))).toEqual({ of: 'c' });
	});

	it('merges by id list', async () => {
		const fetch = mockFetch(json({ entry: 'e1', path: 'a.pdf', size: 1, sidecar: {} }));
		expect((await api.merge(['a', 'b'])).entry).toBe('e1');
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

	it('reads the currencies facet', async () => {
		const facets = {
			areas: [],
			doc_types: [],
			issuers: [],
			tags: [],
			years: [],
			currencies: [{ value: 'JPY', count: 2 }],
		};
		mockFetch(json(facets));
		expect((await api.facets()).currencies).toEqual([{ value: 'JPY', count: 2 }]);
	});

	it('posts an undo', async () => {
		const fetch = mockFetch(json({ entry: { id: 'u' } }));
		await api.undo('e1');
		expect(fetch.mock.calls[0]?.[0]).toBe('/api/activity/e1/undo');
		expect(fetch.mock.calls[0]?.[1]?.method).toBe('POST');
	});

	it('starts a mail sync and forgets a processed mail', async () => {
		const fetch = mockFetch(json({ account: 'home/x', started: true }, 202));
		await api.syncMail('home/x');
		expect(fetch.mock.calls[0]?.[0]).toBe('/api/mail/home%2Fx/sync');
		expect(fetch.mock.calls[0]?.[1]?.method).toBe('POST');
		const forget = mockFetch(json({ id: 7 }));
		await api.reprocessMail(7);
		expect(forget.mock.calls[0]?.[0]).toBe('/api/mail/processed/7/reprocess');
		expect(forget.mock.calls[0]?.[1]?.method).toBe('POST');
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

describe('sessions', () => {
	it('keeps the csrf of a sign-in and sends it on changes only', async () => {
		const login = mockFetch(json(me()));
		await api.login('alex', 'long enough pass');
		expect(login.mock.calls[0]?.[0]).toBe('/api/auth/login');
		expect(JSON.parse(String(login.mock.calls[0]?.[1]?.body))).toEqual({
			name: 'alex',
			password: 'long enough pass',
		});
		expect(header(login.mock.calls[0]?.[1], 'x-csrf-token')).toBeNull();

		const get = mockFetch(json({ total: 0, hits: [] }));
		await api.search();
		expect(header(get.mock.calls[0]?.[1], 'x-csrf-token')).toBeNull();

		const patch = mockFetch(json({ entry: 'e1' }));
		await api.update('abc', { title: 'x' });
		expect(header(patch.mock.calls[0]?.[1], 'x-csrf-token')).toBe('csrf-1');

		const upload = mockFetch(json({ queued: 1 }));
		await api.upload([new File(['a'], 'a.pdf')]);
		expect(header(upload.mock.calls[0]?.[1], 'x-csrf-token')).toBe('csrf-1');
	});

	it('takes the csrf from GET /api/auth and /api/me, and drops it without a session', async () => {
		mockFetch(json({ mode: 'required', setup: false, user: me({ csrf: 'from-auth' }) }));
		await api.auth();
		let post = mockFetch(json({ entry: 'e' }));
		await api.remove('a');
		expect(header(post.mock.calls[0]?.[1], 'x-csrf-token')).toBe('from-auth');

		mockFetch(json(me({ csrf: 'from-me' })));
		await api.me();
		post = mockFetch(json({ entry: 'e' }));
		await api.remove('a');
		expect(header(post.mock.calls[0]?.[1], 'x-csrf-token')).toBe('from-me');

		mockFetch(json({ mode: 'optional', setup: false, user: null }));
		await api.auth();
		post = mockFetch(json({ entry: 'e' }));
		await api.remove('a');
		expect(header(post.mock.calls[0]?.[1], 'x-csrf-token')).toBeNull();
	});

	it('forgets the csrf on sign out and accepts an empty 204', async () => {
		rememberCsrf(me());
		const logout = mockFetch(new Response(null, { status: 204 }));
		await expect(api.logout()).resolves.toBeUndefined();
		expect(logout.mock.calls[0]?.[0]).toBe('/api/auth/logout');
		expect(header(logout.mock.calls[0]?.[1], 'x-csrf-token')).toBe('csrf-1');
		const post = mockFetch(json({ entry: 'e' }));
		await api.remove('a');
		expect(header(post.mock.calls[0]?.[1], 'x-csrf-token')).toBeNull();
	});

	it('sends the user to the sign-in page on a 401, except from the sign-in endpoints', async () => {
		const handler = vi.fn();
		onUnauthorized(handler);
		mockFetch(json({ error: 'sign in' }, 401));
		await expect(api.search()).rejects.toMatchObject({ status: 401 });
		expect(handler).toHaveBeenCalledTimes(1);

		mockFetch(json({ error: 'wrong name or password' }, 401));
		await expect(api.login('alex', 'x')).rejects.toMatchObject({
			message: 'wrong name or password',
		});
		await expect(api.auth()).rejects.toMatchObject({ status: 401 });
		expect(handler).toHaveBeenCalledTimes(1);

		mockFetch(json({ error: 'sign in first' }, 403));
		await expect(api.passkeys()).rejects.toMatchObject({ status: 403 });
		expect(handler).toHaveBeenCalledTimes(1);
	});

	it('reads Retry-After from a 429', async () => {
		mockFetch(
			new Response(JSON.stringify({ error: 'too many attempts' }), {
				status: 429,
				headers: { 'retry-after': '840' },
			}),
		);
		const err = (await api.login('alex', 'x').catch((e: unknown) => e)) as ApiError;
		expect(err.status).toBe(429);
		expect(err.retryAfter).toBe(840);
	});
});

describe('account', () => {
	it('changes the password, with the current one only when given', async () => {
		let fetch = mockFetch(json({ entry: 'e1' }));
		await api.changePassword('new long password', 'old long password');
		expect(fetch.mock.calls[0]?.[0]).toBe('/api/account/password');
		expect(fetch.mock.calls[0]?.[1]?.method).toBe('PUT');
		expect(JSON.parse(String(fetch.mock.calls[0]?.[1]?.body))).toEqual({
			current: 'old long password',
			password: 'new long password',
		});
		fetch = mockFetch(json({ entry: 'e1' }));
		await api.changePassword('new long password');
		expect(JSON.parse(String(fetch.mock.calls[0]?.[1]?.body))).toEqual({
			password: 'new long password',
		});
	});

	it('runs passkey ceremonies and manages passkeys and sessions', async () => {
		const calls: [string, string | undefined, unknown][] = [];
		vi.stubGlobal(
			'fetch',
			vi.fn(async (url: string, init?: RequestInit) => {
				calls.push([url, init?.method, init?.body ? JSON.parse(String(init.body)) : undefined]);
				return url.includes('sessions/') ? new Response(null, { status: 204 }) : json({});
			}),
		);
		await api.passkeyLoginStart('alex');
		await api.passkeyLoginFinish('c1', { id: 'k' });
		await api.passkeyAddStart('phone');
		await api.passkeyAddFinish('c2', { id: 'k' });
		await api.renamePasskey('my phone', 'phone');
		await api.removePasskey('phone');
		await api.endSession('ab12');
		await api.resetPassword('sam', 'long enough pass');
		await api.addUser({ name: 'kim', role: 'member', password: 'long enough pass' });
		expect(calls).toEqual([
			['/api/auth/passkey/start', 'POST', { name: 'alex' }],
			['/api/auth/passkey/finish', 'POST', { ceremony: 'c1', credential: { id: 'k' } }],
			['/api/account/passkeys/start', 'POST', { name: 'phone' }],
			['/api/account/passkeys/finish', 'POST', { ceremony: 'c2', credential: { id: 'k' } }],
			['/api/account/passkeys/my%20phone', 'PATCH', { name: 'phone' }],
			['/api/account/passkeys/phone', 'DELETE', undefined],
			['/api/account/sessions/ab12', 'DELETE', undefined],
			['/api/users/sam/password', 'PUT', { password: 'long enough pass' }],
			['/api/users', 'POST', { name: 'kim', role: 'member', password: 'long enough pass' }],
		]);
	});
});
