import { afterEach, describe, expect, it, vi } from 'vitest';

const kagi = vi.hoisted(() => ({
	autofill: false,
	created: { id: 'new' },
	asserted: { id: 'old' } as { id: string } | null,
}));

vi.mock('@dorsk/kagi', () => ({
	autofillSupported: async () => kagi.autofill,
	createPasskey: async () => kagi.created,
	getPasskey: async () => kagi.asserted,
}));

const { addPasskey, signInWithAutofill, signUp } = await import('./passkeys');

type Call = [string, unknown];

function server(): Call[] {
	const calls: Call[] = [];
	vi.stubGlobal(
		'fetch',
		vi.fn(async (url: string, init?: RequestInit) => {
			calls.push([url, init?.body ? JSON.parse(String(init.body)) : undefined]);
			const body =
				url.endsWith('/start') || url.endsWith('/autofill')
					? { ceremony: 'c', options: { publicKey: {} } }
					: { name: 'alex', passkey: { name: 'phone', created: 'now' }, entry: 'e' };
			return new Response(JSON.stringify(body), {
				headers: { 'content-type': 'application/json' },
			});
		}),
	);
	return calls;
}

afterEach(() => {
	vi.unstubAllGlobals();
	kagi.autofill = false;
	kagi.asserted = { id: 'old' };
});

describe('passkeys', () => {
	it('asks nothing of the server when the browser has no autofill', async () => {
		const calls = server();
		expect(await signInWithAutofill(new AbortController().signal)).toBeNull();
		expect(calls).toEqual([]);
	});

	it('signs in with the passkey picked from autofill, and not after a stop', async () => {
		kagi.autofill = true;
		const calls = server();
		expect(await signInWithAutofill(new AbortController().signal)).toMatchObject({ name: 'alex' });
		expect(calls.map(([url]) => url)).toEqual([
			'/api/auth/passkey/autofill',
			'/api/auth/passkey/finish',
		]);
		kagi.asserted = null;
		const stopped = new AbortController();
		stopped.abort();
		expect(await signInWithAutofill(stopped.signal)).toBeNull();
	});

	it('names the passkey again when it finishes and sends the setup token both times', async () => {
		const calls = server();
		await addPasskey('phone');
		await signUp('alex', 'tok');
		expect(calls).toEqual([
			['/api/account/passkeys/start', { name: 'phone' }],
			['/api/account/passkeys/finish', { ceremony: 'c', credential: { id: 'new' }, name: 'phone' }],
			['/api/auth/setup/start', { name: 'alex', token: 'tok' }],
			['/api/auth/setup/finish', { ceremony: 'c', credential: { id: 'new' }, token: 'tok' }],
		]);
	});
});
