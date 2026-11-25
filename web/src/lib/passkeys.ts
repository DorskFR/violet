import { api, type Me, type Passkey } from './api';
import { createPasskey, usePasskey } from './webauthn';

/** Creates the first account, the admin, with a passkey of this device. */
export async function signUp(name: string): Promise<Me> {
	const { ceremony, options } = await api.setupStart(name);
	return api.setupFinish(ceremony, await createPasskey(options.publicKey));
}

/** Without a name, the device offers the passkeys it holds for violet. */
export async function signInWithPasskey(name?: string): Promise<Me> {
	const { ceremony, options } = await api.passkeyLoginStart(name);
	return api.passkeyLoginFinish(ceremony, await usePasskey(options.publicKey));
}

/** Adds a passkey of this device to the signed-in account; unnamed, it is named after the browser. */
export async function addPasskey(name?: string): Promise<Passkey> {
	const { ceremony, options } = await api.passkeyAddStart(name);
	const { passkey } = await api.passkeyAddFinish(ceremony, await createPasskey(options.publicKey));
	return passkey;
}
