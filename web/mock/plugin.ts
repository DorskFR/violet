import type { IncomingMessage, ServerResponse } from 'node:http';
import type { Plugin } from 'vite';
import type { LoginMode } from '../src/lib/api.ts';
import { type Answer, COOKIE, MockAuth } from './auth.ts';
import { MockConnectors } from './connectors.ts';
import { pagePng } from './png.ts';
import { MockError, MockStore } from './store.ts';

async function readBody(req: IncomingMessage): Promise<Buffer> {
	const chunks: Buffer[] = [];
	for await (const chunk of req) chunks.push(chunk as Buffer);
	return Buffer.concat(chunks);
}

function send(res: ServerResponse, status: number, body: unknown) {
	res.writeHead(status, { 'content-type': 'application/json' }).end(JSON.stringify(body));
}

function answer(res: ServerResponse, out: Answer) {
	const headers: Record<string, string> = {};
	if (out.body !== undefined) headers['content-type'] = 'application/json';
	if (out.retryAfter !== undefined) headers['retry-after'] = String(out.retryAfter);
	if (out.cookie !== undefined) {
		headers['set-cookie'] = out.cookie
			? `${COOKIE}=${out.cookie}; Path=/; HttpOnly; SameSite=Lax`
			: `${COOKIE}=; Path=/; HttpOnly; SameSite=Lax; Max-Age=0`;
	}
	res
		.writeHead(out.status, headers)
		.end(out.body === undefined ? undefined : JSON.stringify(out.body));
}

async function route(
	store: MockStore,
	auth: MockAuth,
	connectors: MockConnectors,
	req: IncomingMessage,
	res: ServerResponse,
) {
	const url = new URL(req.url ?? '/', 'http://mock');
	const method = req.method ?? 'GET';
	const parts = url.pathname.split('/').filter(Boolean).slice(1).map(decodeURIComponent);
	const raw = method === 'GET' ? Buffer.alloc(0) : await readBody(req);
	const json = async () => JSON.parse(raw.toString('utf8') || '{}');
	const key = `${method} ${parts[0] ?? ''}`;

	if (key === 'POST _mock' && parts[1] === 'reset') {
		const body = await json();
		auth.reset({ mode: body.mode as LoginMode | undefined, setup: body.setup === true });
		connectors.reset();
		return answer(res, { status: 204, cookie: null });
	}
	auth.checkCsrf(method, req.headers);
	const accounts = ['auth', 'account', 'me', 'users', 'areas'].includes(parts[0] ?? '');
	const handled = auth.handle(method, parts, req.headers, accounts ? await json() : null);
	if (handled) return answer(res, handled);
	if (!auth.viewer(req.headers)) throw new MockError(401, 'sign in');

	if (key === 'GET status') return send(res, 200, store.status());
	if (key === 'GET search') return send(res, 200, store.search(url.searchParams));
	if (key === 'GET facets') return send(res, 200, store.facets());
	if (key === 'GET duplicates') return send(res, 200, store.duplicates());
	if (key === 'POST merge') return send(res, 200, store.merge((await json()).ids));
	if (key === 'POST upload') {
		const body = raw.toString('latin1');
		return send(res, 200, store.upload(body.split('name="file"').length - 1));
	}
	if (key === 'GET activity') {
		return send(res, 200, store.activity(Number(url.searchParams.get('limit')) || 50));
	}
	if (key === 'POST activity' && parts[2] === 'undo') {
		return send(res, 200, store.undo(parts[1] ?? ''));
	}
	if (parts[0] === 'mail') {
		const me = auth.viewer(req.headers) ?? undefined;
		if (method === 'GET' && !parts[1]) return send(res, 200, connectors.mail(me));
		if (method === 'POST' && parts[1] === 'processed' && parts[3] === 'reprocess') {
			return send(res, 200, connectors.reprocess(Number(parts[2]), me));
		}
	}
	if (parts[0] === 'connectors') {
		const me = auth.viewer(req.headers) ?? undefined;
		const [, name, action, step] = parts;
		if (method === 'GET' && !name) return send(res, 200, connectors.status(me));
		if (method === 'GET' && name === 'kinds') return send(res, 200, connectors.kinds());
		if (method === 'POST' && !name) return send(res, 201, connectors.create(await json(), me));
		if (method === 'POST' && name === 'items' && step === 'refetch') {
			return send(res, 200, connectors.refetch(Number(action)));
		}
		if (method === 'PUT' && name && !action) {
			return send(res, 200, connectors.replace(name, await json(), me));
		}
		if (method === 'DELETE' && name && !action) {
			return send(res, 200, connectors.remove(name, me));
		}
		if (method === 'POST' && name && action === 'run') {
			return send(res, 202, connectors.run(name, me));
		}
		if (method === 'POST' && name && action === 'login') {
			if (!step) return send(res, 200, connectors.login(name, me));
			if (step === 'code') {
				return send(res, 200, connectors.code(name, (await json()).code, me));
			}
		}
	}
	if (parts[0] === 'documents' && parts[1]) {
		const id = parts[1];
		const sub = parts[2];
		if (method === 'GET' && !sub) return send(res, 200, store.document(id));
		if (method === 'PATCH' && !sub) return send(res, 200, store.update(id, await json()));
		if (method === 'DELETE' && !sub) return send(res, 200, store.remove(id));
		if (method === 'GET' && sub === 'pages') {
			const scan = store.scan(id, Number(parts[3]));
			const png = pagePng(scan.seed, Number(url.searchParams.get('w')) || 1200, scan.rotation);
			res.writeHead(200, { 'content-type': 'image/png', 'cache-control': 'no-store' }).end(png);
			return;
		}
		if (method === 'GET' && sub === 'file') {
			const doc = store.document(id);
			const scan = store.scan(id, 1);
			res
				.writeHead(200, {
					'content-type': 'image/png',
					'content-disposition': `inline; filename*=UTF-8''${encodeURIComponent(doc.path.split('/').pop() ?? 'document')}.png`,
				})
				.end(pagePng(scan.seed, 1000, scan.rotation));
			return;
		}
		if (method === 'POST' && sub === 'split')
			return send(res, 200, store.split(id, (await json()).groups));
		if (method === 'POST' && sub === 'rotate') {
			const body = await json();
			return send(res, 200, store.rotate(id, body.pages, body.degrees));
		}
		if (method === 'POST' && sub === 'not-duplicate')
			return send(res, 200, store.notDuplicate(id, (await json()).of));
		if (method === 'POST' && sub === 'reanalyze') return send(res, 200, store.reanalyze(id));
		if (method === 'POST' && sub === 'delete-pages') {
			return send(res, 200, store.deletePages(id, (await json()).pages));
		}
	}
	throw new MockError(404, 'not found');
}

/** Serves `/api` from in-memory fixtures, following docs/api.md, so the UI runs without the server. */
export function mockBackend(): Plugin {
	const store = new MockStore();
	const connectors = new MockConnectors();
	const mode = process.env.VIOLET_MOCK_LOGIN as LoginMode | undefined;
	const auth = new MockAuth((...args) => store.recordSettings(...args), {
		mode,
		setup: process.env.VIOLET_MOCK_SETUP === '1',
	});
	return {
		name: 'violet-mock-backend',
		configureServer(server) {
			server.middlewares.use((req, res, next) => {
				if (!req.url?.startsWith('/api/')) return next();
				route(store, auth, connectors, req, res).catch((e: unknown) => {
					if (e instanceof MockError) send(res, e.status, { error: e.message });
					else send(res, 500, { error: e instanceof Error ? e.message : 'mock backend failed' });
				});
			});
		},
	};
}
