import { describe, expect, it, vi } from 'vitest';
import {
	createPasskey,
	creationOptions,
	credentialJSON,
	fromBase64url,
	passkeyError,
	passkeysSupported,
	requestOptions,
	toBase64url,
	usePasskey,
} from './webauthn';

const bytes = (...values: number[]) => new Uint8Array(values).buffer;
const list = (buffer: BufferSource) =>
	Array.from(
		buffer instanceof ArrayBuffer
			? new Uint8Array(buffer)
			: new Uint8Array(buffer.buffer, buffer.byteOffset, buffer.byteLength),
	);

const creation: PublicKeyCredentialCreationOptionsJSON = {
	rp: { id: 'violet.example', name: 'violet' },
	user: { id: 'AQID', name: 'alex', displayName: 'alex' },
	challenge: '-_8',
	pubKeyCredParams: [{ type: 'public-key', alg: -7 }],
	excludeCredentials: [{ type: 'public-key', id: 'BAU', transports: ['internal'] }],
	authenticatorSelection: { userVerification: 'required' },
	attestation: 'none',
	timeout: 60000,
};

const request: PublicKeyCredentialRequestOptionsJSON = {
	challenge: 'AAEC',
	rpId: 'violet.example',
	allowCredentials: [{ type: 'public-key', id: 'BAU' }],
	userVerification: 'required',
};

describe('base64url', () => {
	it('round-trips bytes that need the url-safe alphabet, without padding', () => {
		const data = bytes(251, 255, 191, 0, 1);
		const text = toBase64url(data);
		expect(text).toBe('-_-_AAE');
		expect(list(fromBase64url(text))).toEqual([251, 255, 191, 0, 1]);
	});

	it('reads standard base64 and padding too', () => {
		expect(list(fromBase64url('+/8='))).toEqual([251, 255]);
		expect(list(fromBase64url(''))).toEqual([]);
	});

	it('encodes a view on part of a buffer', () => {
		const view = new Uint8Array([9, 1, 2, 3, 9]).subarray(1, 4);
		expect(toBase64url(view)).toBe('AQID');
	});
});

describe('options without the browser parsers', () => {
	it('decodes the challenge, the user id and the excluded credentials', () => {
		const options = creationOptions(creation);
		expect(list(options.challenge)).toEqual([251, 255]);
		expect(list(options.user.id)).toEqual([1, 2, 3]);
		expect(list(options.excludeCredentials?.[0]?.id as BufferSource)).toEqual([4, 5]);
		expect(options.excludeCredentials?.[0]?.transports).toEqual(['internal']);
		expect(options.rp).toEqual(creation.rp);
		expect(options.attestation).toBe('none');
		expect(options.authenticatorSelection?.userVerification).toBe('required');
	});

	it('decodes the challenge and allowed credentials of a sign-in', () => {
		const options = requestOptions(request);
		expect(list(options.challenge)).toEqual([0, 1, 2]);
		expect(list(options.allowCredentials?.[0]?.id as BufferSource)).toEqual([4, 5]);
		expect(options.rpId).toBe('violet.example');
		expect(options.userVerification).toBe('required');
	});

	it('uses the browser parsers when there are some', () => {
		const parsed = { challenge: bytes(1) } as PublicKeyCredentialCreationOptions;
		const parse = vi.fn(() => parsed);
		expect(creationOptions(creation, { parseCreationOptionsFromJSON: parse })).toBe(parsed);
		expect(parse).toHaveBeenCalledWith(creation);
	});
});

function fakeCredential(response: object, toJSON?: () => unknown): PublicKeyCredential {
	return {
		id: 'BAU',
		rawId: bytes(4, 5),
		type: 'public-key',
		authenticatorAttachment: 'platform',
		response,
		getClientExtensionResults: () => ({ credProps: { rk: true } }),
		...(toJSON && { toJSON }),
	} as unknown as PublicKeyCredential;
}

describe('credentialJSON', () => {
	it('encodes a registration by hand when toJSON is missing', () => {
		const credential = fakeCredential({
			clientDataJSON: bytes(1),
			attestationObject: bytes(2),
			getTransports: () => ['internal', 'hybrid'],
		});
		expect(credentialJSON(credential)).toEqual({
			id: 'BAU',
			rawId: 'BAU',
			type: 'public-key',
			authenticatorAttachment: 'platform',
			response: {
				clientDataJSON: 'AQ',
				attestationObject: 'Ag',
				transports: ['internal', 'hybrid'],
			},
			clientExtensionResults: { credProps: { rk: true } },
		});
	});

	it('encodes an assertion by hand, with or without a user handle', () => {
		const assertion = (userHandle: ArrayBuffer | null) =>
			fakeCredential({
				clientDataJSON: bytes(1),
				authenticatorData: bytes(2),
				signature: bytes(3),
				userHandle,
			});
		expect(credentialJSON(assertion(bytes(7)))).toMatchObject({
			response: {
				clientDataJSON: 'AQ',
				authenticatorData: 'Ag',
				signature: 'Aw',
				userHandle: 'Bw',
			},
		});
		expect(credentialJSON(assertion(null))).toMatchObject({ response: { userHandle: null } });
	});

	it('prefers the browser toJSON', () => {
		const credential = fakeCredential({ clientDataJSON: bytes(1) }, () => ({ native: true }));
		expect(credentialJSON(credential)).toEqual({ native: true });
	});
});

describe('ceremonies', () => {
	it('creates a passkey with decoded options and answers its JSON', async () => {
		const create = vi.fn(async (_: CredentialCreationOptions) =>
			fakeCredential({ clientDataJSON: bytes(1), attestationObject: bytes(2) }),
		);
		const out = await createPasskey(creation, { credentials: { create, get: vi.fn() } });
		expect(list(create.mock.calls[0]?.[0].publicKey?.challenge as BufferSource)).toEqual([
			251, 255,
		]);
		expect(out).toMatchObject({ id: 'BAU', response: { attestationObject: 'Ag' } });
	});

	it('signs in with a passkey', async () => {
		const get = vi.fn(async (_: CredentialRequestOptions) =>
			fakeCredential({
				clientDataJSON: bytes(1),
				authenticatorData: bytes(2),
				signature: bytes(3),
				userHandle: null,
			}),
		);
		const out = await usePasskey(request, { credentials: { create: vi.fn(), get } });
		expect(list(get.mock.calls[0]?.[0].publicKey?.challenge as BufferSource)).toEqual([0, 1, 2]);
		expect(out).toMatchObject({ response: { signature: 'Aw' } });
	});

	it('says when the browser has no passkey support', async () => {
		expect(passkeysSupported({})).toBe(false);
		await expect(usePasskey(request, {})).rejects.toThrow('This browser cannot use passkeys.');
	});

	it('explains a cancelled prompt in plain words', () => {
		expect(passkeyError(new DOMException('x', 'NotAllowedError'), 'f')).toBe(
			'The passkey request was cancelled or timed out.',
		);
		expect(passkeyError(new Error('boom'), 'f')).toBe('boom');
		expect(passkeyError('?', 'f')).toBe('f');
	});
});
