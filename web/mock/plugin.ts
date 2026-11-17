import type { IncomingMessage, ServerResponse } from 'node:http';
import type { Plugin } from 'vite';
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

async function route(store: MockStore, req: IncomingMessage, res: ServerResponse) {
	const url = new URL(req.url ?? '/', 'http://mock');
	const method = req.method ?? 'GET';
	const parts = url.pathname.split('/').filter(Boolean).slice(1).map(decodeURIComponent);
	const json = async () => JSON.parse((await readBody(req)).toString('utf8') || '{}');
	const key = `${method} ${parts[0] ?? ''}`;

	if (key === 'GET status') return send(res, 200, store.status());
	if (key === 'GET search') return send(res, 200, store.search(url.searchParams));
	if (key === 'GET facets') return send(res, 200, store.facets());
	if (key === 'POST merge') return send(res, 200, store.merge((await json()).ids));
	if (key === 'POST upload') {
		const body = (await readBody(req)).toString('latin1');
		return send(res, 200, store.upload(body.split('name="file"').length - 1));
	}
	if (key === 'GET activity') {
		return send(res, 200, store.activity(Number(url.searchParams.get('limit')) || 50));
	}
	if (key === 'POST activity' && parts[2] === 'undo') {
		return send(res, 200, store.undo(parts[1] ?? ''));
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
	return {
		name: 'violet-mock-backend',
		configureServer(server) {
			server.middlewares.use((req, res, next) => {
				if (!req.url?.startsWith('/api/')) return next();
				route(store, req, res).catch((e: unknown) => {
					if (e instanceof MockError) send(res, e.status, { error: e.message });
					else send(res, 500, { error: e instanceof Error ? e.message : 'mock backend failed' });
				});
			});
		},
	};
}
