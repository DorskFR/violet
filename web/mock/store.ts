import type {
	Action,
	ActivityItem,
	Changed,
	Document,
	DocumentPatch,
	Entry,
	Facet,
	Facets,
	Hit,
	Job,
	SearchResult,
	Sidecar,
	Status,
} from '../src/lib/api.ts';
import { buildLibrary, libraryPath, type MockDocument, sha, uuid } from './fixtures.ts';

export class MockError extends Error {
	constructor(
		public readonly status: number,
		message: string,
	) {
		super(message);
	}
}

type Logged = { entry: Entry; undone: boolean; revert?: () => void };

const escapeHtml = (text: string) =>
	text
		.replace(/&/g, '&amp;')
		.replace(/</g, '&lt;')
		.replace(/>/g, '&gt;')
		.replace(/"/g, '&quot;')
		.replace(/'/g, '&#39;');

function snippet(text: string, terms: string[]): string | null {
	const flat = text.replace(/\s+/g, ' ');
	const lower = flat.toLowerCase();
	const at = terms.map((t) => lower.indexOf(t)).find((i) => i >= 0);
	if (at === undefined) return null;
	const start = Math.max(0, at - 30);
	const slice = flat.slice(start, at + 90);
	const pattern = new RegExp(
		terms.map((t) => t.replace(/[.*+?^${}()|[\]\\]/g, '\\$&')).join('|'),
		'gi',
	);
	let out = start > 0 ? '…' : '';
	let last = 0;
	for (const m of slice.matchAll(pattern)) {
		out += `${escapeHtml(slice.slice(last, m.index))}<mark>${escapeHtml(m[0])}</mark>`;
		last = m.index + m[0].length;
	}
	return `${out}${escapeHtml(slice.slice(last))}${start + slice.length < flat.length ? '…' : ''}`;
}

function count(values: string[], byCount = true): Facet[] {
	const map = new Map<string, number>();
	for (const v of values) map.set(v, (map.get(v) ?? 0) + 1);
	const out = [...map].map(([value, n]) => ({ value, count: n }));
	return out.sort((a, b) =>
		byCount ? b.count - a.count || a.value.localeCompare(b.value) : b.value.localeCompare(a.value),
	);
}

export class MockStore {
	private docs = new Map<string, MockDocument>();
	private log: Logged[] = [];
	private jobs: Job[] = [];
	private inbox = 2;
	private clock: number;

	constructor(now = new Date()) {
		this.clock = now.getTime();
		const library = buildLibrary(now);
		for (const doc of library) this.docs.set(doc.sidecar.id, doc);
		for (const doc of [...library].reverse()) {
			const source = doc.sidecar.sources[0];
			this.record(
				'agent',
				{
					kind: 'ingest',
					original: `${source?.sha256.slice(0, 12)}.pdf`,
					original_name: source?.original_name ?? 'scan.pdf',
					documents: [[doc.sidecar.id, doc.path]],
				},
				`Filed "${doc.sidecar.title}" under ${doc.sidecar.area}`,
				() => this.docs.delete(doc.sidecar.id),
				new Date(doc.sidecar.created_at),
			);
		}
		const first = library[0];
		if (first) {
			this.record(
				'agent',
				{
					kind: 'duplicate',
					original: 'a81c33f0e2d1.pdf',
					original_name: 'IMG_4471.pdf',
					of: first.sidecar.id,
				},
				`"IMG_4471.pdf" is a copy of "${first.sidecar.title}"; kept in originals`,
				undefined,
				new Date(this.clock - 40 * 60_000),
			);
		}
		const stamp = (minutes: number) => new Date(this.clock - minutes * 60_000).toISOString();
		this.jobs = [
			{
				id: 412,
				kind: 'ingest',
				key: sha(9001),
				payload: { path: 'スキャン_2026-11-15.pdf' },
				state: 'failed',
				attempts: 3,
				error: 'vision model timed out after 120s on page 7 of 12',
				updated_at: stamp(95),
			},
			{
				id: 409,
				kind: 'ingest',
				key: sha(9002),
				payload: { path: 'encrypted-statement.pdf' },
				state: 'failed',
				attempts: 1,
				error: 'the PDF is password protected',
				updated_at: stamp(600),
			},
		];
	}

	private now(): string {
		this.clock += 1000;
		return new Date(this.clock).toISOString();
	}

	private record(
		actor: string,
		action: Action,
		summary: string,
		revert?: () => void,
		at?: Date,
	): Entry {
		const entry: Entry = {
			id: uuid(),
			at: at ? at.toISOString() : this.now(),
			actor,
			action,
			summary,
		};
		this.log.push({ entry, undone: false, revert });
		return entry;
	}

	private need(id: string): MockDocument {
		const doc = this.docs.get(id);
		if (!doc) throw new MockError(404, 'no such document');
		return doc;
	}

	private view(doc: MockDocument): Document {
		return structuredClone({ path: doc.path, size: doc.size, sidecar: doc.sidecar });
	}

	status(): Status {
		const failed = this.jobs.filter((j) => j.state === 'failed');
		return {
			version: '0.1.0-mock',
			inbox: this.inbox,
			jobs: {
				pending: this.inbox,
				running: this.inbox > 0 ? 1 : 0,
				done: this.docs.size,
				failed: failed.length,
			},
			failed,
			model: { vision: true, text: false },
		};
	}

	facets(): Facets {
		const all = [...this.docs.values()].map((d) => d.sidecar);
		return {
			areas: count(all.map((s) => s.area)),
			doc_types: count(all.map((s) => s.doc_type)),
			issuers: count(all.flatMap((s) => (s.issuer ? [s.issuer] : []))),
			tags: count(all.flatMap((s) => s.tags)),
			years: count(
				all.flatMap((s) => (s.date ? [s.date.slice(0, 4)] : [])),
				false,
			),
			currencies: count(
				all.flatMap((s) => [...new Set(s.facts.flatMap((f) => (f.currency ? [f.currency] : [])))]),
			),
		};
	}

	search(params: URLSearchParams): SearchResult {
		const get = (key: string) => params.get(key) ?? '';
		const terms = get('q').toLowerCase().split(/\s+/).filter(Boolean);
		const tags = params.getAll('tag');
		const num = (key: string) => (params.has(key) ? Number(params.get(key)) : null);
		const [min, max, currency] = [num('min'), num('max'), get('currency')];
		const matches: { doc: MockDocument; snippet: string | null }[] = [];
		for (const doc of this.docs.values()) {
			const s = doc.sidecar;
			if (get('area') && s.area !== get('area')) continue;
			if (get('type') && s.doc_type !== get('type')) continue;
			if (get('issuer') && s.issuer !== get('issuer')) continue;
			if (!tags.every((t) => s.tags.includes(t))) continue;
			if (get('from') && !(s.date && s.date >= get('from'))) continue;
			if (get('to') && !(s.date && s.date <= get('to'))) continue;
			if (min !== null || max !== null) {
				const hit = s.facts.some(
					(f) =>
						f.amount_minor !== null &&
						(!currency || f.currency === currency) &&
						(min === null || f.amount_minor >= min) &&
						(max === null || f.amount_minor <= max),
				);
				if (!hit) continue;
			}
			const body = s.pages.map((p) => p.text).join(' ');
			const haystack = [s.title, s.issuer ?? '', s.tags.join(' '), s.summary ?? '', body]
				.join(' ')
				.toLowerCase();
			if (!terms.every((t) => haystack.includes(t))) continue;
			matches.push({ doc, snippet: terms.length ? snippet(body, terms) : null });
		}
		matches.sort((a, b) => (b.doc.sidecar.date ?? '').localeCompare(a.doc.sidecar.date ?? ''));
		const offset = Number(get('offset')) || 0;
		const limit = Number(get('limit')) || 50;
		const hits: Hit[] = matches.slice(offset, offset + limit).map(({ doc, snippet }) => {
			const s = doc.sidecar;
			const amounts = s.facts.filter((f) => f.amount_minor !== null);
			const main =
				amounts.find((f) => f.key === 'total') ??
				[...amounts].sort((a, b) => (b.amount_minor ?? 0) - (a.amount_minor ?? 0))[0];
			return {
				id: s.id,
				path: doc.path,
				title: s.title,
				area: s.area,
				doc_type: s.doc_type,
				issuer: s.issuer,
				date: s.date,
				tags: s.tags,
				amount_minor: main?.amount_minor ?? null,
				currency: main?.currency ?? null,
				pages: s.pages.length,
				snippet,
			};
		});
		return { total: matches.length, hits };
	}

	document(id: string): Document {
		return this.view(this.need(id));
	}

	scan(id: string, page: number): { seed: number; rotation: number } {
		const scan = this.need(id).scans[page - 1];
		if (!scan) throw new MockError(404, 'no such page');
		return scan;
	}

	update(id: string, patch: DocumentPatch): Changed<Document> {
		const doc = this.need(id);
		if (patch.title !== undefined && !patch.title.trim()) {
			throw new MockError(422, 'title must not be empty');
		}
		const before = structuredClone(doc);
		const fields = Object.entries(patch).filter(([, value]) => value !== undefined);
		const same = fields.every(
			([key, value]) => JSON.stringify(doc.sidecar[key]) === JSON.stringify(value),
		);
		if (same) return { entry: null, ...this.view(doc) };
		Object.assign(doc.sidecar, Object.fromEntries(fields), {
			confidence: 1,
			updated_at: this.now(),
		});
		doc.path = libraryPath(doc.sidecar);
		const entry = this.record(
			'user',
			{ kind: 'update', document: id, from: before.path, to: doc.path, before: before.sidecar },
			`Edited "${doc.sidecar.title}"`,
			() => this.docs.set(id, before),
		);
		return { entry: entry.id, ...this.view(doc) };
	}

	private derive(from: MockDocument, pages: number[], title: string): MockDocument {
		const sidecar: Sidecar = {
			...structuredClone(from.sidecar),
			id: uuid(),
			title,
			pages: pages.map((n, i) => ({
				...(from.sidecar.pages[n - 1] ?? { text: '', source: 'none' as const }),
				number: i + 1,
			})),
			sha256: sha(this.clock + pages.length),
			updated_at: this.now(),
		};
		return {
			path: libraryPath(sidecar),
			size: 48_000 + pages.length * 131_000,
			sidecar,
			scans: pages.map((n) => ({ ...(from.scans[n - 1] ?? { seed: n, rotation: 0 }) })),
		};
	}

	private checkPages(doc: MockDocument, pages: unknown): number[] {
		const total = doc.sidecar.pages.length;
		if (
			!Array.isArray(pages) ||
			pages.length === 0 ||
			!pages.every((p) => Number.isInteger(p) && p >= 1 && p <= total)
		) {
			throw new MockError(422, `pages must be between 1 and ${total}`);
		}
		return pages as number[];
	}

	split(id: string, groups: number[][]): Changed<{ documents: string[] }> {
		const doc = this.need(id);
		if (!Array.isArray(groups) || groups.length < 2) {
			throw new MockError(422, 'a split needs at least two groups');
		}
		const made = groups.map((group, i) =>
			this.derive(
				doc,
				this.checkPages(doc, group),
				`${doc.sidecar.title} (${i + 1}/${groups.length})`,
			),
		);
		this.docs.delete(id);
		for (const d of made) this.docs.set(d.sidecar.id, d);
		const entry = this.record(
			'user',
			{
				kind: 'split',
				original: `${doc.sidecar.sha256.slice(0, 12)}.pdf`,
				from: [id, doc.path],
				into: made.map((d) => [d.sidecar.id, d.path]),
			},
			`Split "${doc.sidecar.title}" into ${made.length} documents`,
			() => {
				for (const d of made) this.docs.delete(d.sidecar.id);
				this.docs.set(id, doc);
			},
		);
		return { entry: entry.id, documents: made.map((d) => d.sidecar.id) };
	}

	rotate(id: string, pages: number[], degrees: number): Changed<Document> {
		const doc = this.need(id);
		if (![90, 180, 270].includes(degrees))
			throw new MockError(422, 'degrees must be 90, 180 or 270');
		const before = structuredClone(doc);
		for (const n of this.checkPages(doc, pages)) {
			const scan = doc.scans[n - 1];
			if (scan) scan.rotation = (scan.rotation + degrees) % 360;
		}
		const entry = this.touch(
			doc,
			before,
			`rotated ${pages.length === 1 ? `page ${pages[0]}` : `${pages.length} pages`} by ${degrees}°`,
		);
		return { entry, ...this.view(doc) };
	}

	deletePages(id: string, pages: number[]): Changed<Document> {
		const doc = this.need(id);
		const drop = new Set(this.checkPages(doc, pages));
		if (drop.size >= doc.sidecar.pages.length) {
			throw new MockError(422, 'cannot delete every page; delete the document instead');
		}
		const before = structuredClone(doc);
		doc.scans = doc.scans.filter((_, i) => !drop.has(i + 1));
		doc.sidecar.pages = doc.sidecar.pages
			.filter((p) => !drop.has(p.number))
			.map((p, i) => ({ ...p, number: i + 1 }));
		const entry = this.touch(
			doc,
			before,
			`deleted ${drop.size === 1 ? `page ${pages[0]}` : `${drop.size} pages`}`,
		);
		return { entry, ...this.view(doc) };
	}

	private touch(doc: MockDocument, before: MockDocument, detail: string): string {
		doc.sidecar.sha256 = sha(this.clock);
		doc.sidecar.updated_at = this.now();
		return this.record(
			'user',
			{
				kind: 'edit_pages',
				original: `${before.sidecar.sha256.slice(0, 12)}.pdf`,
				document: [doc.sidecar.id, doc.path],
				detail,
			},
			`"${doc.sidecar.title}": ${detail}`,
			() => this.docs.set(doc.sidecar.id, before),
		).id;
	}

	remove(id: string): { entry: string } {
		const doc = this.need(id);
		this.docs.delete(id);
		const entry = this.record(
			'user',
			{
				kind: 'delete',
				original: `${doc.sidecar.sha256.slice(0, 12)}.pdf`,
				document: [id, doc.path],
				sidecar: structuredClone(doc.sidecar),
			},
			`Deleted "${doc.sidecar.title}"`,
			() => this.docs.set(id, doc),
		);
		return { entry: entry.id };
	}

	merge(ids: string[]): Changed<Document> {
		if (!Array.isArray(ids) || new Set(ids).size < 2) {
			throw new MockError(422, 'a merge needs at least two documents');
		}
		const parts = ids.map((id) => this.need(id));
		const head = parts[0] as MockDocument;
		const merged = this.derive(head, [], head.sidecar.title);
		merged.sidecar.pages = parts
			.flatMap((d) => d.sidecar.pages)
			.map((p, i) => ({ ...p, number: i + 1 }));
		merged.sidecar.tags = [...new Set(parts.flatMap((d) => d.sidecar.tags))];
		merged.sidecar.facts = parts.flatMap((d) => d.sidecar.facts);
		merged.sidecar.sources = parts.flatMap((d) => d.sidecar.sources);
		merged.scans = parts.flatMap((d) => d.scans.map((s) => ({ ...s })));
		merged.size = parts.reduce((sum, d) => sum + d.size, 0);
		for (const d of parts) this.docs.delete(d.sidecar.id);
		this.docs.set(merged.sidecar.id, merged);
		const entry = this.record(
			'user',
			{
				kind: 'merge',
				originals: parts.map((d) => [`${d.sidecar.sha256.slice(0, 12)}.pdf`, d.sidecar.id, d.path]),
				into: [merged.sidecar.id, merged.path],
			},
			`Merged ${parts.length} documents into "${merged.sidecar.title}"`,
			() => {
				this.docs.delete(merged.sidecar.id);
				for (const d of parts) this.docs.set(d.sidecar.id, d);
			},
		);
		return { entry: entry.id, ...this.view(merged) };
	}

	upload(files: number): { queued: number } {
		this.inbox += files;
		return { queued: files };
	}

	activity(limit: number): ActivityItem[] {
		return [...this.log]
			.sort((a, b) => b.entry.at.localeCompare(a.entry.at))
			.slice(0, limit)
			.map(({ entry, undone, revert }) => ({
				entry: structuredClone(entry),
				undone,
				undoable: !undone && revert !== undefined,
			}));
	}

	undo(entryId: string): { entry: Entry } {
		const logged = this.log.find((l) => l.entry.id === entryId);
		if (!logged) throw new MockError(404, 'no such entry');
		if (logged.undone || !logged.revert) throw new MockError(409, 'this entry cannot be undone');
		logged.revert();
		logged.undone = true;
		const entry = this.record(
			'user',
			{ kind: 'undo', entry: entryId },
			`Undid: ${logged.entry.summary}`,
		);
		return { entry };
	}
}
