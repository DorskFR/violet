export type Parsers = {
	parseCreationOptionsFromJSON?: (
		json: PublicKeyCredentialCreationOptionsJSON,
	) => PublicKeyCredentialCreationOptions;
	parseRequestOptionsFromJSON?: (
		json: PublicKeyCredentialRequestOptionsJSON,
	) => PublicKeyCredentialRequestOptions;
};

export type Authenticator = {
	parsers?: Parsers;
	credentials?: Pick<CredentialsContainer, 'create' | 'get'>;
};

export function toBase64url(data: ArrayBuffer | ArrayBufferView): string {
	const bytes =
		data instanceof ArrayBuffer
			? new Uint8Array(data)
			: new Uint8Array(data.buffer, data.byteOffset, data.byteLength);
	let binary = '';
	for (const byte of bytes) binary += String.fromCharCode(byte);
	return btoa(binary).replace(/\+/g, '-').replace(/\//g, '_').replace(/=+$/, '');
}

/** Also reads standard base64 and padded input. */
export function fromBase64url(text: string): ArrayBuffer {
	const b64 = text.replace(/-/g, '+').replace(/_/g, '/').replace(/=+$/, '');
	const binary = atob(b64 + '='.repeat((4 - (b64.length % 4)) % 4));
	const out = new Uint8Array(binary.length);
	for (let i = 0; i < binary.length; i++) out[i] = binary.charCodeAt(i);
	return out.buffer;
}

const descriptor = (d: PublicKeyCredentialDescriptorJSON): PublicKeyCredentialDescriptor => ({
	...d,
	type: d.type as PublicKeyCredentialType,
	id: fromBase64url(d.id),
	transports: d.transports as AuthenticatorTransport[] | undefined,
});

export function creationOptions(
	json: PublicKeyCredentialCreationOptionsJSON,
	parsers: Parsers = {},
): PublicKeyCredentialCreationOptions {
	if (parsers.parseCreationOptionsFromJSON) return parsers.parseCreationOptionsFromJSON(json);
	const { extensions: _, ...rest } = json;
	return {
		...rest,
		attestation: json.attestation as AttestationConveyancePreference | undefined,
		challenge: fromBase64url(json.challenge),
		user: { ...json.user, id: fromBase64url(json.user.id) },
		excludeCredentials: json.excludeCredentials?.map(descriptor),
	};
}

export function requestOptions(
	json: PublicKeyCredentialRequestOptionsJSON,
	parsers: Parsers = {},
): PublicKeyCredentialRequestOptions {
	if (parsers.parseRequestOptionsFromJSON) return parsers.parseRequestOptionsFromJSON(json);
	const { extensions: _, ...rest } = json;
	return {
		...rest,
		userVerification: json.userVerification as UserVerificationRequirement | undefined,
		challenge: fromBase64url(json.challenge),
		allowCredentials: json.allowCredentials?.map(descriptor),
	};
}

type AnyResponse = AuthenticatorResponse &
	Partial<AuthenticatorAttestationResponse> &
	Partial<AuthenticatorAssertionResponse>;

export function credentialJSON(credential: PublicKeyCredential): unknown {
	if (typeof credential.toJSON === 'function') return credential.toJSON();
	const r = credential.response as AnyResponse;
	const response: Record<string, unknown> = { clientDataJSON: toBase64url(r.clientDataJSON) };
	if (r.attestationObject) {
		response.attestationObject = toBase64url(r.attestationObject);
		if (typeof r.getTransports === 'function') response.transports = r.getTransports();
	}
	if (r.authenticatorData) response.authenticatorData = toBase64url(r.authenticatorData);
	if (r.signature) response.signature = toBase64url(r.signature);
	if (r.userHandle !== undefined) {
		response.userHandle = r.userHandle ? toBase64url(r.userHandle) : null;
	}
	return {
		id: credential.id,
		rawId: toBase64url(credential.rawId),
		type: credential.type,
		authenticatorAttachment: credential.authenticatorAttachment ?? null,
		response,
		clientExtensionResults: credential.getClientExtensionResults(),
	};
}

function browser(): Authenticator {
	if (typeof PublicKeyCredential === 'undefined' || typeof navigator === 'undefined') return {};
	return {
		parsers: PublicKeyCredential as unknown as Parsers,
		credentials: navigator.credentials,
	};
}

export function passkeysSupported(env: Authenticator = browser()): boolean {
	return env.credentials !== undefined;
}

function need(env: Authenticator): Pick<CredentialsContainer, 'create' | 'get'> {
	if (!env.credentials) throw new Error('This browser cannot use passkeys.');
	return env.credentials;
}

/** Runs the registration ceremony and answers what `…/passkeys/finish` takes as `credential`. */
export async function createPasskey(
	options: PublicKeyCredentialCreationOptionsJSON,
	env: Authenticator = browser(),
): Promise<unknown> {
	const credential = await need(env).create({ publicKey: creationOptions(options, env.parsers) });
	if (!credential) throw new Error('No passkey was created.');
	return credentialJSON(credential as PublicKeyCredential);
}

export async function usePasskey(
	options: PublicKeyCredentialRequestOptionsJSON,
	env: Authenticator = browser(),
): Promise<unknown> {
	const credential = await need(env).get({ publicKey: requestOptions(options, env.parsers) });
	if (!credential) throw new Error('No passkey was chosen.');
	return credentialJSON(credential as PublicKeyCredential);
}

export function passkeyError(e: unknown, fallback: string): string {
	if (e instanceof DOMException) {
		if (e.name === 'NotAllowedError') return 'The passkey request was cancelled or timed out.';
		if (e.name === 'InvalidStateError') return 'This device already holds a passkey for you.';
		if (e.name === 'SecurityError') return 'Passkeys do not work at this address.';
	}
	return e instanceof Error && e.message ? e.message : fallback;
}
