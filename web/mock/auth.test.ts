import { describe, expect, it } from 'vitest';
import type { AuthState, Me, Session } from '../src/lib/api.ts';
import { COOKIE, MOCK_PASSWORD, MockAuth } from './auth.ts';
import { MockError, MockStore } from './store.ts';

function setup(options: ConstructorParameters<typeof MockAuth>[1] = {}) {
	const store = new MockStore();
	const auth = new MockAuth((...args) => store.recordSettings(...args), options);
	const call = (method: string, path: string, body: unknown = null, headers = {}) => {
		const parts = path.split('/').filter(Boolean);
		auth.checkCsrf(method, headers);
		const out = auth.handle(method, parts, headers, body);
		if (!out) throw new Error(`unhandled ${path}`);
		return out;
	};
	const login = (name = 'alex', password = MOCK_PASSWORD) => {
		const out = call('POST', 'auth/login', { name, password });
		const me = out.body as Me;
		const headers = { cookie: `${COOKIE}=${out.cookie}`, 'x-csrf-token': me.csrf ?? '' };
		return { me, headers };
	};
	return { store, auth, call, login };
}

const fails = (fn: () => unknown, status: number) => {
	try {
		fn();
	} catch (e) {
		expect(e).toBeInstanceOf(MockError);
		expect((e as MockError).status).toBe(status);
		return;
	}
	throw new Error(`expected ${status}`);
};

const clientData = (type: string, challenge: string) =>
	Buffer.from(JSON.stringify({ type, challenge, origin: 'http://localhost:5173' })).toString(
		'base64url',
	);

describe('mock accounts', () => {
	it('runs as the implicit admin when optional, and signs in with a csrf', () => {
		const { auth, call, login } = setup();
		expect(auth.viewer({})?.via).toBe('implicit');
		const { me, headers } = login();
		expect(me).toMatchObject({
			name: 'alex',
			via: 'session',
			password: true,
			passkeys: ['laptop'],
		});
		expect(me.csrf).toBeTruthy();
		expect((call('GET', 'me', null, headers).body as Me).name).toBe('alex');
		fails(() => call('DELETE', 'users/sam', null, { cookie: headers.cookie }), 403);
		expect(call('DELETE', 'users/sam', null, headers).status).toBe(200);
	});

	it('answers 401 when required, and offers setup only before any credential', () => {
		const { auth, call } = setup({ mode: 'required' });
		expect(auth.viewer({})).toBeNull();
		expect((call('GET', 'auth').body as AuthState).setup).toBe(false);
		fails(() => call('POST', 'auth/setup', { name: 'x', password: 'long enough pass' }), 409);

		const fresh = setup({ mode: 'required', setup: true });
		expect(fresh.auth.viewer({})?.via).toBe('implicit');
		expect((fresh.call('GET', 'auth').body as AuthState).setup).toBe(true);
		fails(() => fresh.call('POST', 'auth/setup', { name: 'alex', password: 'short' }), 400);
		const out = fresh.call('POST', 'auth/setup', { name: 'alex', password: 'long enough pass' });
		expect(out.cookie).toBeTruthy();
		expect((out.body as Me).role).toBe('admin');
		expect(fresh.auth.viewer({})).toBeNull();
		expect(fresh.store.activity(5)[0]?.entry.action.kind).toBe('settings');
	});

	it('answers 404 for sign-in endpoints when off', () => {
		const { call } = setup({ mode: 'off' });
		expect((call('GET', 'auth').body as AuthState).mode).toBe('off');
		fails(() => call('POST', 'auth/login', { name: 'alex', password: MOCK_PASSWORD }), 404);
	});

	it('refuses a name after five failures with Retry-After', () => {
		const { call } = setup();
		for (let i = 0; i < 5; i++) {
			fails(() => call('POST', 'auth/login', { name: 'alex', password: 'wrong password!' }), 401);
		}
		const out = call('POST', 'auth/login', { name: 'alex', password: MOCK_PASSWORD });
		expect(out.status).toBe(429);
		expect(out.retryAfter).toBe(900);
	});

	it('ends the other sessions on a password change and gives this one a new cookie', () => {
		const { auth, call, login } = setup();
		const a = login();
		const b = login();
		fails(
			() =>
				call(
					'PUT',
					'account/password',
					{ current: 'nope', password: 'brand new password' },
					a.headers,
				),
			403,
		);
		const out = call(
			'PUT',
			'account/password',
			{ current: MOCK_PASSWORD, password: 'brand new password' },
			a.headers,
		);
		expect(out.cookie).toBeTruthy();
		expect(auth.viewer(b.headers)?.via).toBe('implicit');
		const fresh = auth.viewer({ cookie: `${COOKIE}=${out.cookie}` });
		expect(fresh?.csrf).toBe(a.me.csrf);
		const freshHeaders = { cookie: `${COOKIE}=${out.cookie}`, 'x-csrf-token': a.me.csrf ?? '' };
		fails(
			() => call('PUT', 'account/password', { password: 'other new password' }, freshHeaders),
			400,
		);
		expect(auth.viewer({ cookie: `${COOKIE}=${out.cookie}` })?.name).toBe('alex');
	});

	it('lists sessions, marks the current one and ends another', () => {
		const { auth, call, login } = setup();
		const a = login();
		const b = login();
		const list = call('GET', 'account/sessions', null, a.headers).body as Session[];
		expect(list).toHaveLength(2);
		const other = list.find((s) => !s.current);
		expect(call('DELETE', `account/sessions/${other?.id}`, null, a.headers).status).toBe(204);
		expect(auth.viewer(b.headers)?.via).toBe('implicit');
	});

	it('registers a passkey, signs in with it, and keeps the last credential', () => {
		const { auth, call, login } = setup();
		const sam = login('sam');
		const start = call('POST', 'account/passkeys/start', { name: 'phone' }, sam.headers).body as {
			ceremony: string;
			options: { publicKey: { challenge: string } };
		};
		const credential = {
			id: 'c2Ftcw',
			response: {
				clientDataJSON: clientData('webauthn.create', start.options.publicKey.challenge),
			},
		};
		call('POST', 'account/passkeys/finish', { ceremony: start.ceremony, credential }, sam.headers);
		expect((call('GET', 'me', null, sam.headers).body as Me).passkeys).toEqual(['phone']);

		const get = call('POST', 'auth/passkey/start', { name: 'sam' }).body as typeof start;
		const signed = call('POST', 'auth/passkey/finish', {
			ceremony: get.ceremony,
			credential: {
				id: 'c2Ftcw',
				response: { clientDataJSON: clientData('webauthn.get', get.options.publicKey.challenge) },
			},
		});
		const headers = {
			cookie: `${COOKIE}=${signed.cookie}`,
			'x-csrf-token': (signed.body as Me).csrf ?? '',
		};
		expect(auth.viewer(headers)?.name).toBe('sam');
		fails(() => call('POST', 'auth/passkey/finish', { ceremony: get.ceremony, credential }), 401);
		try {
			call('POST', 'auth/passkey/start', { name: 'nobody' });
		} catch (e) {
			expect((e as MockError).message).toBe('wrong name or passkey');
		}
		fails(
			() => call('PUT', 'users/alex/password', { password: 'z'.repeat(12) }, login().headers),
			409,
		);

		call('PUT', 'users/sam/password', { password: 'x'.repeat(12) }, login().headers);
		expect(auth.viewer(sam.headers)?.via).toBe('implicit');
		expect(auth.viewer(headers)?.name).toBe('sam');
	});

	it('refuses to remove the last way to sign in', () => {
		const { call } = setup();
		const get = call('POST', 'auth/passkey/start', { name: 'kim' }).body as {
			ceremony: string;
			options: { publicKey: { challenge: string } };
		};
		const signed = call('POST', 'auth/passkey/finish', {
			ceremony: get.ceremony,
			credential: {
				id: 'bW9jay1raW0',
				response: { clientDataJSON: clientData('webauthn.get', get.options.publicKey.challenge) },
			},
		});
		const me = signed.body as Me;
		expect(me).toMatchObject({ name: 'kim', password: false, passkeys: ['phone'] });
		const headers = { cookie: `${COOKIE}=${signed.cookie}`, 'x-csrf-token': me.csrf ?? '' };
		fails(() => call('DELETE', 'account/passkeys/phone', null, headers), 409);
		expect(call('PATCH', 'account/passkeys/phone', { name: 'pixel' }, headers).status).toBe(200);
		fails(() => call('POST', 'auth/login', { name: 'kim', password: MOCK_PASSWORD }), 401);
	});

	it('undoes a users change through the journal', () => {
		const { store, call, login } = setup();
		const out = call('DELETE', 'users/sam', null, login().headers);
		const entry = (out.body as { entry: string }).entry;
		store.undo(entry);
		expect(call('POST', 'auth/login', { name: 'sam', password: MOCK_PASSWORD }).status).toBe(200);
	});

	it('answers 403 on account endpoints for the implicit admin', () => {
		const { call } = setup();
		fails(() => call('GET', 'account/passkeys'), 403);
	});
});
