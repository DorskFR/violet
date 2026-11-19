import { randomBytes } from 'node:crypto';
import type {
	Areas,
	AuthState,
	LoginMode,
	Me,
	Passkey,
	Role,
	Scope,
	Session,
	TokenInfo,
	Users,
} from '../src/lib/api.ts';
import { MockError } from './store.ts';

export const MOCK_PASSWORD = 'violet-mock-password';

type MockPasskey = Passkey & { id: string };

type MockUser = {
	name: string;
	role: Role;
	password: string | null;
	passkeys: MockPasskey[];
	tokens: TokenInfo[];
};

type MockSession = {
	id: string;
	user: string;
	credential: 'password' | 'passkey';
	key: string;
	created: string;
	last_seen: string;
	expires: string;
	label: string;
	csrf: string;
};

type Ceremony = { user: string; challenge: string; kind: 'create' | 'get'; name?: string };

export type Headers = Record<string, string | string[] | undefined>;

/** What a handled request answers; `cookie` is the session token to set, `null` to clear it. */
export type Answer = {
	status: number;
	body?: unknown;
	cookie?: string | null;
	retryAfter?: number;
};

export type Journal = (actor: string, file: string, summary: string, revert: () => void) => string;

export const COOKIE = 'violet_mock_session';

const random = (n: number) => randomBytes(n).toString('base64url');

const one = (value: string | string[] | undefined) => (Array.isArray(value) ? value[0] : value);

function cookieToken(headers: Headers): string | undefined {
	for (const part of (one(headers.cookie) ?? '').split(';')) {
		const [k, ...v] = part.trim().split('=');
		if (k === COOKIE) return v.join('=');
	}
	return undefined;
}

function label(agent: string | undefined): string {
	const ua = agent ?? '';
	const browser = /Firefox\//.test(ua)
		? 'Firefox'
		: /Edg\//.test(ua)
			? 'Edge'
			: /Chrome\//.test(ua)
				? 'Chrome'
				: /Safari\//.test(ua)
					? 'Safari'
					: 'Browser';
	const os = /Android/.test(ua)
		? 'Android'
		: /iPhone|iPad/.test(ua)
			? 'iOS'
			: /Mac OS X/.test(ua)
				? 'macOS'
				: /Windows/.test(ua)
					? 'Windows'
					: /Linux/.test(ua)
						? 'Linux'
						: 'unknown system';
	return `${browser} on ${os}`;
}

function clientData(credential: unknown): { type?: string; challenge?: string } {
	const raw = (credential as { response?: { clientDataJSON?: string } })?.response?.clientDataJSON;
	if (typeof raw !== 'string') throw new MockError(400, 'the credential has no client data');
	try {
		return JSON.parse(Buffer.from(raw, 'base64url').toString('utf8'));
	} catch {
		throw new MockError(400, 'the client data is not JSON');
	}
}

function str(body: unknown, key: string): string {
	const value = (body as Record<string, unknown> | null)?.[key];
	if (typeof value !== 'string') throw new MockError(400, `${key} is required`);
	return value;
}

function checkPassword(password: string, name: string) {
	const bytes = Buffer.byteLength(password);
	if (bytes < 12 || bytes > 1024) throw new MockError(400, 'a password has 12 to 1024 bytes');
	if (password === name) throw new MockError(400, 'the password cannot be the user name');
}

/**
 * Accounts, sessions and passkeys for the dev mock, after docs/design/multi-user.md. Passwords are
 * kept in clear and passkey signatures are not checked: it only has to drive the UI.
 */
export class MockAuth {
	mode: LoginMode;
	private users: MockUser[] = [];
	private configured = false;
	private areas: Areas = { private: [] };
	private sessions = new Map<string, MockSession>();
	private ceremonies = new Map<string, Ceremony>();
	private failures = new Map<string, number>();
	private now: () => Date;

	constructor(
		private readonly journal: Journal,
		options: { mode?: LoginMode; setup?: boolean; now?: () => Date } = {},
	) {
		this.mode = options.mode ?? 'optional';
		this.now = options.now ?? (() => new Date());
		if (!options.setup) this.seed();
	}

	private seed() {
		const created = new Date(this.now().getTime() - 40 * 86400_000).toISOString();
		this.users = [
			{
				name: 'alex',
				role: 'admin',
				password: MOCK_PASSWORD,
				passkeys: [{ name: 'laptop', created, id: 'bW9jay1sYXB0b3A' }],
				tokens: [],
			},
			{
				name: 'sam',
				role: 'member',
				password: MOCK_PASSWORD,
				passkeys: [],
				tokens: [{ name: 'scanner', scope: 'write', expires: null }],
			},
			{
				name: 'kim',
				role: 'member',
				password: null,
				passkeys: [{ name: 'phone', created, id: 'bW9jay1raW0' }],
				tokens: [],
			},
		];
		this.configured = true;
		this.areas = { private: [{ area: 'Health', members: ['alex', 'sam'] }] };
	}

	reset(options: { mode?: LoginMode; setup?: boolean }) {
		this.mode = options.mode ?? 'optional';
		this.users = [];
		this.configured = false;
		this.areas = { private: [] };
		this.sessions.clear();
		this.ceremonies.clear();
		this.failures.clear();
		if (!options.setup) this.seed();
	}

	private get setup(): boolean {
		return !this.users.some((u) => u.password !== null || u.passkeys.length > 0);
	}

	private key(user: MockUser, credential: MockSession['credential'], id?: string): string {
		return credential === 'password' ? `password:${user.password}` : `passkey:${id}`;
	}

	private valid(session: MockSession): MockUser | undefined {
		const user = this.users.find((u) => u.name === session.user);
		if (!user) return undefined;
		const ok =
			session.key === `password:${user.password}` ||
			user.passkeys.some((p) => session.key === `passkey:${p.id}`);
		if (!ok || new Date(session.expires) <= this.now()) return undefined;
		return user;
	}

	private session(headers: Headers): [string, MockSession, MockUser] | undefined {
		if (this.mode === 'off') return undefined;
		const token = cookieToken(headers);
		const session = token ? this.sessions.get(token) : undefined;
		if (!token || !session) return undefined;
		const user = this.valid(session);
		if (!user) {
			this.sessions.delete(token);
			return undefined;
		}
		return [token, session, user];
	}

	private meOf(user: MockUser, session: MockSession | null): Me {
		return {
			csrf: session?.csrf ?? null,
			name: user.name,
			role: user.role,
			scope: user.role === 'admin' ? 'admin' : ('write' as Scope),
			via: session ? 'session' : 'token',
			areas: this.areas.private.filter((a) => a.members.includes(user.name)).map((a) => a.area),
			users: this.configured,
			password: user.password !== null,
			passkeys: user.passkeys.map((p) => p.name),
		};
	}

	private implicit(): Me {
		return {
			csrf: null,
			name: null,
			role: 'admin',
			scope: 'admin',
			via: 'implicit',
			areas: [],
			users: this.configured,
			password: false,
			passkeys: [],
		};
	}

	/** Who the request runs as, or null when it must answer 401. */
	viewer(headers: Headers): Me | null {
		const found = this.session(headers);
		if (found) return this.meOf(found[2], found[1]);
		if (this.mode === 'optional') return this.implicit();
		if (this.mode === 'required' && this.setup) return this.implicit();
		if (this.mode === 'off' && !this.configured) return this.implicit();
		return null;
	}

	/** A changing request signed in by the cookie must carry its CSRF token. */
	checkCsrf(method: string, headers: Headers) {
		if (method === 'GET' || method === 'HEAD') return;
		const found = this.session(headers);
		if (found && one(headers['x-csrf-token']) !== found[1].csrf) {
			throw new MockError(403, 'missing or wrong CSRF token');
		}
	}

	private open(
		user: MockUser,
		credential: MockSession['credential'],
		headers: Headers,
		id?: string,
	) {
		const now = this.now();
		const token = random(32);
		const session: MockSession = {
			id: random(6),
			user: user.name,
			credential,
			key: this.key(user, credential, id),
			created: now.toISOString(),
			last_seen: now.toISOString(),
			expires: new Date(now.getTime() + 30 * 86400_000).toISOString(),
			label: label(one(headers['user-agent'])),
			csrf: random(32),
		};
		this.sessions.set(token, session);
		return { status: 200, body: this.meOf(user, session), cookie: token };
	}

	private actor(me: Me): string {
		return me.name ? `user:${me.name}` : 'user';
	}

	private change(me: Me, file: string, summary: string, apply: () => void): string {
		const before = structuredClone({ users: this.users, areas: this.areas, c: this.configured });
		apply();
		return this.journal(this.actor(me), file, summary, () => {
			this.users = before.users;
			this.areas = before.areas;
			this.configured = before.c;
		});
	}

	private need(name: string): MockUser {
		const user = this.users.find((u) => u.name === name);
		if (!user) throw new MockError(404, 'no such user');
		return user;
	}

	private admin(me: Me | null): Me {
		if (!me) throw new MockError(401, 'sign in');
		if (me.role !== 'admin' || me.scope !== 'admin') throw new MockError(403, 'admins only');
		return me;
	}

	private failed(name: string) {
		this.failures.set(name, (this.failures.get(name) ?? 0) + 1);
	}

	/** Answers the auth, account, me, users and areas endpoints; null for any other path. */
	handle(method: string, parts: string[], headers: Headers, body: unknown): Answer | null {
		const [root, a, b, c] = parts;
		const me = this.viewer(headers);
		const off = this.mode === 'off';

		if (root === 'auth') {
			if (method === 'GET' && !a) {
				const state: AuthState = { mode: this.mode, setup: this.setup, user: me };
				return { status: 200, body: state };
			}
			if (off) throw new MockError(404, 'not found');
			if (method === 'POST' && a === 'setup') {
				if (!this.setup) throw new MockError(409, 'an account already exists');
				if (me?.via !== 'implicit') throw new MockError(403, 'only the implicit admin can do this');
				const name = str(body, 'name').trim();
				const password = str(body, 'password');
				if (!name) throw new MockError(400, 'name is required');
				checkPassword(password, name);
				const user: MockUser = { name, role: 'admin', password, passkeys: [], tokens: [] };
				this.change(me, '.users.json', `Created the admin account ${name}`, () => {
					this.users = [...this.users.filter((u) => u.name !== name), user];
					this.configured = true;
				});
				return this.open(user, 'password', headers);
			}
			if (method === 'POST' && a === 'login') {
				const name = str(body, 'name');
				if ((this.failures.get(name) ?? 0) >= 5) {
					return { status: 429, body: { error: 'too many attempts' }, retryAfter: 900 };
				}
				const user = this.users.find((u) => u.name === name);
				if (!user?.password || user.password !== str(body, 'password')) {
					this.failed(name);
					throw new MockError(401, 'wrong name or password');
				}
				this.failures.delete(name);
				return this.open(user, 'password', headers);
			}
			if (method === 'POST' && a === 'passkey' && b === 'start') {
				const user = this.users.find((u) => u.name === str(body, 'name'));
				if (!user || user.passkeys.length === 0) {
					throw new MockError(401, 'wrong name or passkey');
				}
				const challenge = random(32);
				const ceremony = random(16);
				this.ceremonies.set(ceremony, { user: user.name, challenge, kind: 'get' });
				return {
					status: 200,
					body: {
						ceremony,
						options: {
							publicKey: {
								challenge,
								timeout: 60000,
								rpId: 'localhost',
								allowCredentials: user.passkeys.map((p) => ({ type: 'public-key', id: p.id })),
								userVerification: 'required',
							},
						},
					},
				};
			}
			if (method === 'POST' && a === 'passkey' && b === 'finish') {
				const ceremony = this.ceremonies.get(str(body, 'ceremony'));
				this.ceremonies.delete(str(body, 'ceremony'));
				const credential = (body as { credential?: { id?: string } }).credential;
				const user = ceremony && this.users.find((u) => u.name === ceremony.user);
				const data = clientData(credential);
				const key = user?.passkeys.find((p) => p.id === credential?.id);
				if (
					ceremony?.kind !== 'get' ||
					!user ||
					!key ||
					data.type !== 'webauthn.get' ||
					data.challenge !== ceremony.challenge
				) {
					throw new MockError(401, 'wrong name or passkey');
				}
				return this.open(user, 'passkey', headers, key.id);
			}
			if (method === 'POST' && a === 'logout') {
				const token = cookieToken(headers);
				if (token) this.sessions.delete(token);
				return { status: 204, cookie: null };
			}
			throw new MockError(404, 'not found');
		}

		if (root === 'account') {
			if (off) throw new MockError(404, 'not found');
			const found = this.session(headers);
			if (!found) {
				if (me) throw new MockError(403, 'sign in first');
				throw new MockError(401, 'sign in');
			}
			const [token, session, user] = found;
			const self = this.meOf(user, session);
			if (method === 'PUT' && a === 'password') {
				const password = str(body, 'password');
				if (user.password !== null) {
					const current = (body as { current?: unknown }).current;
					if (typeof current !== 'string') throw new MockError(400, 'current is required');
					if (current !== user.password) throw new MockError(403, 'wrong password');
				}
				checkPassword(password, user.name);
				const entry = this.change(
					self,
					'.users.json',
					`Changed the password of ${user.name}`,
					() => {
						user.password = password;
					},
				);
				this.sessions.delete(token);
				const fresh = this.open(user, 'password', headers);
				const kept = this.sessions.get(fresh.cookie);
				if (kept) kept.csrf = session.csrf;
				return { status: 200, body: { entry }, cookie: fresh.cookie };
			}
			if (a === 'passkeys') {
				if (method === 'GET' && !b) {
					return {
						status: 200,
						body: user.passkeys.map(({ name, created }) => ({ name, created })),
					};
				}
				if (method === 'POST' && b === 'start') {
					const name = str(body, 'name').trim();
					if (!name) throw new MockError(400, 'name is required');
					if (user.passkeys.some((p) => p.name === name)) {
						throw new MockError(409, 'a passkey already has this name');
					}
					const challenge = random(32);
					const ceremony = random(16);
					this.ceremonies.set(ceremony, { user: user.name, challenge, kind: 'create', name });
					return {
						status: 200,
						body: {
							ceremony,
							options: {
								publicKey: {
									rp: { id: 'localhost', name: 'violet' },
									user: {
										id: Buffer.from(user.name).toString('base64url'),
										name: user.name,
										displayName: user.name,
									},
									challenge,
									pubKeyCredParams: [
										{ type: 'public-key', alg: -7 },
										{ type: 'public-key', alg: -257 },
									],
									timeout: 60000,
									attestation: 'none',
									authenticatorSelection: {
										residentKey: 'preferred',
										userVerification: 'required',
									},
									excludeCredentials: user.passkeys.map((p) => ({ type: 'public-key', id: p.id })),
								},
							},
						},
					};
				}
				if (method === 'POST' && b === 'finish') {
					const ceremony = this.ceremonies.get(str(body, 'ceremony'));
					this.ceremonies.delete(str(body, 'ceremony'));
					const credential = (body as { credential?: { id?: string } }).credential;
					const data = clientData(credential);
					if (
						!ceremony?.name ||
						ceremony.kind !== 'create' ||
						ceremony.user !== user.name ||
						typeof credential?.id !== 'string' ||
						data.type !== 'webauthn.create' ||
						data.challenge !== ceremony.challenge
					) {
						throw new MockError(400, 'the passkey was not accepted');
					}
					const passkey = { name: ceremony.name, created: this.now().toISOString() };
					const id = credential.id;
					const entry = this.change(self, '.users.json', `Added a passkey for ${user.name}`, () => {
						user.passkeys.push({ ...passkey, id });
					});
					return { status: 200, body: { entry, passkey } };
				}
				if (b && !c) {
					const key = user.passkeys.find((p) => p.name === b);
					if (!key) throw new MockError(404, 'no such passkey');
					if (method === 'PATCH') {
						const to = str(body, 'name').trim();
						if (!to) throw new MockError(400, 'name is required');
						if (user.passkeys.some((p) => p.name === to && p !== key)) {
							throw new MockError(409, 'a passkey already has this name');
						}
						const entry = this.change(
							self,
							'.users.json',
							`Renamed a passkey of ${user.name}`,
							() => {
								key.name = to;
							},
						);
						return { status: 200, body: { entry } };
					}
					if (method === 'DELETE') {
						if (user.password === null && user.passkeys.length === 1) {
							throw new MockError(409, 'this is the last way to sign in to this account');
						}
						const entry = this.change(
							self,
							'.users.json',
							`Removed a passkey of ${user.name}`,
							() => {
								user.passkeys = user.passkeys.filter((p) => p !== key);
							},
						);
						return { status: 200, body: { entry } };
					}
				}
			}
			if (a === 'sessions') {
				const mine = [...this.sessions].filter(([, s]) => s.user === user.name && this.valid(s));
				if (method === 'GET' && !b) {
					const list: Session[] = mine.map(([t, s]) => ({
						id: s.id,
						label: s.label,
						created: s.created,
						last_seen: s.last_seen,
						expires: s.expires,
						credential: s.credential,
						current: t === token,
					}));
					return { status: 200, body: list };
				}
				if (method === 'DELETE' && b) {
					const hit = mine.find(([, s]) => s.id === b);
					if (!hit) throw new MockError(404, 'no such session');
					this.sessions.delete(hit[0]);
					return { status: 204, ...(hit[0] === token && { cookie: null }) };
				}
			}
			throw new MockError(404, 'not found');
		}

		if (root === 'me' && method === 'GET' && !a) {
			if (!me) throw new MockError(401, 'sign in');
			return { status: 200, body: me };
		}

		if (root === 'areas') {
			if (method === 'GET') {
				if (!me) throw new MockError(401, 'sign in');
				return { status: 200, body: this.areas };
			}
			if (method === 'PUT') {
				const by = this.admin(me);
				const next = body as Areas;
				const entry = this.change(by, '.areas.json', 'Changed the private areas', () => {
					this.areas = { private: next.private ?? [] };
				});
				return { status: 200, body: { entry } };
			}
		}

		if (root === 'users') {
			const by = this.admin(me);
			if (method === 'GET' && !a) {
				const users: Users = {
					configured: this.configured,
					users: this.users.map((u) => ({
						name: u.name,
						role: u.role,
						password: u.password !== null,
						passkeys: u.passkeys.map((p) => p.name),
						tokens: u.tokens,
					})),
				};
				return { status: 200, body: users };
			}
			if (method === 'POST' && !a) {
				const name = str(body, 'name').trim();
				const role = str(body, 'role') as Role;
				const password = (body as { password?: unknown }).password;
				if (!name) throw new MockError(400, 'name is required');
				if (this.users.some((u) => u.name === name)) throw new MockError(409, 'user exists');
				if (password !== undefined) checkPassword(str(body, 'password'), name);
				const entry = this.change(by, '.users.json', `Added the user ${name}`, () => {
					this.users.push({
						name,
						role,
						password: typeof password === 'string' ? password : null,
						passkeys: [],
						tokens: [],
					});
					this.configured = true;
				});
				return { status: 200, body: { entry } };
			}
			if (!a) throw new MockError(404, 'not found');
			const user = this.need(a);
			if (method === 'PATCH' && !b) {
				const role = str(body, 'role') as Role;
				if (user.name === by.name && role !== 'admin') {
					throw new MockError(409, 'you would no longer be an admin');
				}
				const entry = this.change(by, '.users.json', `Changed the role of ${a}`, () => {
					user.role = role;
				});
				return { status: 200, body: { entry } };
			}
			if (method === 'DELETE' && !b) {
				if (user.name === by.name) throw new MockError(409, 'you would no longer be an admin');
				const entry = this.change(by, '.users.json', `Removed the user ${a}`, () => {
					this.users = this.users.filter((u) => u !== user);
				});
				return { status: 200, body: { entry } };
			}
			if (method === 'PUT' && b === 'password') {
				if (user.name === by.name) {
					throw new MockError(409, 'change your own password under your account');
				}
				const password = str(body, 'password');
				checkPassword(password, user.name);
				const entry = this.change(by, '.users.json', `Set the password of ${a}`, () => {
					user.password = password;
				});
				return { status: 200, body: { entry } };
			}
			if (method === 'POST' && b === 'tokens') {
				const name = str(body, 'name').trim();
				const scope = str(body, 'scope') as Scope;
				const expires = (body as { expires?: string }).expires ?? null;
				const secret = `vt_${random(24)}`;
				const entry = this.change(by, '.users.json', `Added the token ${name} for ${a}`, () => {
					user.tokens.push({ name, scope, expires });
				});
				return { status: 200, body: { token: secret, entry } };
			}
			if (method === 'DELETE' && b === 'tokens' && c) {
				const entry = this.change(by, '.users.json', `Revoked the token ${c} of ${a}`, () => {
					user.tokens = user.tokens.filter((t) => t.name !== c);
				});
				return { status: 200, body: { entry } };
			}
		}
		return null;
	}
}
