import { autofillSupported, createPasskey, getPasskey } from '@dorsk/kagi';
import { api, type Me, type Passkey } from './api';

/** Creates the first account, the admin, with a passkey of this device and the setup token. */
export async function signUp(name: string, token: string): Promise<Me> {
	const { ceremony, options } = await api.setupStart(name, token);
	return api.setupFinish(ceremony, await createPasskey(options), token);
}

/** Without a name, the device offers the passkeys it holds for violet. */
export async function signInWithPasskey(name?: string): Promise<Me> {
	const { ceremony, options } = await api.passkeyLoginStart(name);
	const credential = await getPasskey(options);
	if (!credential) throw new Error('No passkey was chosen.');
	return api.passkeyLoginFinish(ceremony, credential);
}

/**
 * Offers the passkeys of this device in the name field's autofill until one is picked, then
 * signs in with it. Answers null when the browser has no autofill or `signal` stops it.
 */
export async function signInWithAutofill(signal: AbortSignal): Promise<Me | null> {
	if (!(await autofillSupported())) return null;
	const { ceremony, options } = await api.passkeyAutofillStart();
	if (signal.aborted) return null;
	const credential = await getPasskey(options, { signal });
	return credential ? api.passkeyLoginFinish(ceremony, credential) : null;
}

/** Adds a passkey of this device to the signed-in account; unnamed, it is named after the browser. */
export async function addPasskey(name?: string): Promise<Passkey> {
	const { ceremony, options } = await api.passkeyAddStart(name);
	const credential = await createPasskey(options);
	const { passkey } = await api.passkeyAddFinish(ceremony, credential, name);
	return passkey;
}
