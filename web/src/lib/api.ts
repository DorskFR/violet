export type TextSource = 'layer' | 'ocr' | 'extracted' | 'none';

export type Fact = {
	key: string;
	value: string;
	/** Minor units of `currency`; JPY has none, so 1200 JPY is 1200. */
	amount_minor: number | null;
	currency: string | null;
};

export type PageText = { number: number; text: string; source: TextSource };

export type MailSource = {
	account: string;
	folder: string;
	uidvalidity: number;
	uid: number;
	message_id: string | null;
	from: string | null;
	subject: string | null;
	date: string | null;
};

export type Source = {
	sha256: string;
	original_name: string;
	pages: number[];
	mail?: MailSource;
};

export type Sidecar = {
	version: number;
	id: string;
	title: string;
	area: string;
	doc_type: string;
	issuer: string | null;
	/** `YYYY-MM-DD`. */
	date: string | null;
	language: string | null;
	summary: string | null;
	tags: string[];
	facts: Fact[];
	pages: PageText[];
	sources: Source[];
	sha256: string;
	confidence: number;
	created_at: string;
	updated_at: string;
	[extra: string]: unknown;
};

export type Hit = {
	id: string;
	path: string;
	title: string;
	area: string;
	doc_type: string;
	issuer: string | null;
	date: string | null;
	tags: string[];
	amount_minor: number | null;
	currency: string | null;
	pages: number;
	/** HTML holding only `<mark>` tags. Render through `parseSnippet`, never as raw HTML. */
	snippet: string | null;
};

export type UnderstoodTerm = { text: string; kind: 'date' | 'amount' | 'latest' };

/** The filters the server read from `q`; `terms` are the parts of `q` it read them from. */
export type Understood = {
	from?: string;
	to?: string;
	month?: number;
	min?: number;
	max?: number;
	currency?: string;
	latest?: boolean;
	terms: UnderstoodTerm[];
};

/** `partial`: no document has every word, these have some. */
export type SearchResult = {
	total: number;
	hits: Hit[];
	partial?: boolean;
	understood?: Understood;
};

export type Facet = { value: string; count: number };

export type Facets = {
	areas: Facet[];
	doc_types: Facet[];
	issuers: Facet[];
	tags: Facet[];
	years: Facet[];
	currencies: Facet[];
};

export type DocRef = [id: string, path: string];

export type Action =
	| { kind: 'ingest'; original: string; original_name: string; documents: DocRef[] }
	| { kind: 'duplicate'; original: string; original_name: string; of: string }
	| { kind: 'split'; original: string; from: DocRef; into: DocRef[] }
	| { kind: 'merge'; originals: [original: string, id: string, path: string][]; into: DocRef }
	| { kind: 'edit_pages'; original: string; document: DocRef; detail: string }
	| { kind: 'update'; document: string; from: string; to: string; before: Sidecar }
	| { kind: 'delete'; original: string; document: DocRef; sidecar: Sidecar }
	| {
			kind: 'replace';
			original: string;
			document: string;
			from: string;
			to: string;
			before: Sidecar;
	  }
	| { kind: 'undo'; entry: string };

export type Entry = {
	id: string;
	at: string;
	actor: 'agent' | 'user' | (string & {});
	on_behalf_of?: string;
	action: Action;
	summary: string;
};

export type ActivityItem = { entry: Entry; undone: boolean; undoable: boolean };

export type JobState = 'pending' | 'running' | 'done' | 'failed';

export type Job = {
	id: number;
	kind: 'ingest';
	key: string;
	payload: { path?: string; [key: string]: unknown };
	state: JobState;
	attempts: number;
	error: string | null;
	updated_at: string;
};

export type Status = {
	version: string;
	inbox: number;
	jobs: Record<JobState, number>;
	failed: Job[];
	model: { vision: boolean; text: boolean };
};

export type MailReport = {
	mails: number;
	ingested: number;
	files: number;
	skipped: number;
	failed: number;
	errors: string[];
};

export type MailAccount = {
	name: string;
	host: string;
	folders: string[];
	interval_secs: number;
	running: boolean;
	last_run: string | null;
	last_success: string | null;
	last_error: string | null;
	last_report: MailReport | null;
	counts: { ingested: number; skipped: number; failed: number };
};

export type ProcessedMail = {
	id: number;
	account: string;
	folder: string;
	uidvalidity: number;
	uid: number;
	message_id: string | null;
	sender: string | null;
	subject: string | null;
	received: string | null;
	status: 'ingested' | 'skipped' | 'failed';
	files: number;
	detail: string | null;
	error: string | null;
	processed_at: string;
};

export type MailStatus = {
	enabled: boolean;
	accounts: MailAccount[];
	processed: ProcessedMail[];
};

export type Document = { path: string; size: number; sidecar: Sidecar };

export type Role = 'member' | 'admin';

export type Scope = 'read' | 'write' | 'admin';

/** `users: false`: no users file, every request runs as the implicit admin with `name: null`. */
export type Me = {
	name: string | null;
	role: Role;
	scope: Scope;
	via: 'implicit' | 'certificate' | 'token' | 'default' | 'local';
	areas: string[];
	users: boolean;
};

export type TokenInfo = { name: string; scope: Scope; expires: string | null };

export type User = { name: string; role: Role; identities: string[]; tokens: TokenInfo[] };

export type Users = { configured: boolean; users: User[] };

export type PrivateArea = { area: string; members: string[] };

export type Areas = { private: PrivateArea[] };

/** `entry` is the journal entry to undo the change with; `null` when nothing had to change. */
export type Changed<T> = T & { entry: string | null };

export type DocumentPatch = Partial<
	Pick<Sidecar, 'title' | 'area' | 'doc_type' | 'issuer' | 'date' | 'tags' | 'facts'>
>;

export type SearchParams = {
	q?: string;
	area?: string;
	type?: string;
	issuer?: string;
	tag?: string[];
	from?: string;
	to?: string;
	/** Minor units. */
	min?: number;
	max?: number;
	currency?: string;
	limit?: number;
	offset?: number;
};

export class ApiError extends Error {
	constructor(
		public readonly status: number,
		message: string,
	) {
		super(message);
		this.name = 'ApiError';
	}
}

export async function parseError(res: Response): Promise<ApiError> {
	const fallback = res.statusText || `Request failed (${res.status})`;
	const text = await res.text().catch(() => '');
	if (!text) return new ApiError(res.status, fallback);
	try {
		const body: unknown = JSON.parse(text);
		if (body && typeof body === 'object' && 'error' in body && typeof body.error === 'string') {
			return new ApiError(res.status, body.error || fallback);
		}
	} catch {
		return new ApiError(res.status, fallback);
	}
	return new ApiError(res.status, fallback);
}

async function request<T>(method: string, path: string, body?: unknown): Promise<T> {
	const init: RequestInit = { method, headers: { accept: 'application/json' } };
	if (body instanceof FormData) {
		init.body = body;
	} else if (body !== undefined) {
		init.headers = { ...init.headers, 'content-type': 'application/json' };
		init.body = JSON.stringify(body);
	}
	let res: Response;
	try {
		res = await fetch(path, init);
	} catch {
		throw new ApiError(0, 'The server could not be reached.');
	}
	if (!res.ok) throw await parseError(res);
	return (await res.json()) as T;
}

export function searchQuery(params: SearchParams): string {
	const out = new URLSearchParams();
	const set = (key: string, value: string | number | undefined) => {
		if (value !== undefined && value !== '') out.set(key, String(value));
	};
	set('q', params.q?.trim());
	set('area', params.area);
	set('type', params.type);
	set('issuer', params.issuer);
	for (const tag of params.tag ?? []) out.append('tag', tag);
	set('from', params.from);
	set('to', params.to);
	set('min', params.min);
	set('max', params.max);
	set('currency', params.currency);
	set('limit', params.limit);
	set('offset', params.offset);
	return out.toString();
}

const doc = (id: string) => `/api/documents/${encodeURIComponent(id)}`;

const user = (name: string) => `/api/users/${encodeURIComponent(name)}`;

export function pageUrl(id: string, page: number, width: number, version?: string): string {
	const v = version ? `&v=${encodeURIComponent(version)}` : '';
	return `${doc(id)}/pages/${page}?w=${width}${v}`;
}

export function pageSrcset(
	id: string,
	page: number,
	widths: readonly number[],
	version?: string,
): string {
	return widths.map((w) => `${pageUrl(id, page, w, version)} ${w}w`).join(', ');
}

export function fileUrl(id: string): string {
	return `${doc(id)}/file`;
}

export const api = {
	status: () => request<Status>('GET', '/api/status'),
	search: (params: SearchParams = {}) => {
		const query = searchQuery(params);
		return request<SearchResult>('GET', query ? `/api/search?${query}` : '/api/search');
	},
	facets: () => request<Facets>('GET', '/api/facets'),
	document: (id: string) => request<Document>('GET', doc(id)),
	update: (id: string, patch: DocumentPatch) => request<Changed<Document>>('PATCH', doc(id), patch),
	split: (id: string, groups: number[][]) =>
		request<Changed<{ documents: string[] }>>('POST', `${doc(id)}/split`, { groups }),
	rotate: (id: string, pages: number[], degrees: 90 | 180 | 270) =>
		request<Changed<Document>>('POST', `${doc(id)}/rotate`, { pages, degrees }),
	deletePages: (id: string, pages: number[]) =>
		request<Changed<Document>>('POST', `${doc(id)}/delete-pages`, { pages }),
	reanalyze: (id: string) => request<Changed<Document>>('POST', `${doc(id)}/reanalyze`),
	remove: (id: string) => request<{ entry: string }>('DELETE', doc(id)),
	merge: (ids: string[]) => request<Changed<Document>>('POST', '/api/merge', { ids }),
	upload: (files: File[]) => {
		const form = new FormData();
		for (const file of files) form.append('file', file, file.name);
		return request<{ queued: number }>('POST', '/api/upload', form);
	},
	activity: (limit = 50) => request<ActivityItem[]>('GET', `/api/activity?limit=${limit}`),
	undo: (entryId: string) =>
		request<{ entry: Entry }>('POST', `/api/activity/${encodeURIComponent(entryId)}/undo`),
	mail: () => request<MailStatus>('GET', '/api/mail'),
	syncMail: (account: string) =>
		request<{ account: string; started: boolean }>(
			'POST',
			`/api/mail/${encodeURIComponent(account)}/sync`,
		),
	reprocessMail: (id: number) =>
		request<ProcessedMail>('POST', `/api/mail/processed/${id}/reprocess`),
	me: () => request<Me>('GET', '/api/me'),
	users: () => request<Users>('GET', '/api/users'),
	addUser: (body: { name: string; role: Role; identities?: string[] }) =>
		request<{ entry: string }>('POST', '/api/users', body),
	updateUser: (name: string, patch: { role?: Role; identities?: string[] }) =>
		request<{ entry: string }>('PATCH', user(name), patch),
	removeUser: (name: string) => request<{ entry: string }>('DELETE', user(name)),
	addToken: (name: string, token: { name: string; scope: Scope; expires?: string }) =>
		request<{ token: string; entry: string }>('POST', `${user(name)}/tokens`, token),
	revokeToken: (name: string, token: string) =>
		request<{ entry: string }>('DELETE', `${user(name)}/tokens/${encodeURIComponent(token)}`),
	areas: () => request<Areas>('GET', '/api/areas'),
	saveAreas: (areas: Areas) => request<{ entry: string }>('PUT', '/api/areas', areas),
};

export function errorMessage(e: unknown, fallback: string): string {
	return e instanceof Error && e.message ? e.message : fallback;
}
