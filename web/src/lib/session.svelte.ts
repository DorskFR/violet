import { api, type Me, rememberCsrf } from './api';
import type { Login } from './auth';

export const session = $state<{ me: Me | null; login: Login | null }>({ me: null, login: null });

export function signedInAs(me: Me | null): void {
	rememberCsrf(me);
	session.me = me;
}

export function loginIs(login: Login): void {
	session.login = { mode: login.mode, setup: login.setup };
}

export async function loadLogin(): Promise<void> {
	try {
		loginIs(await api.auth());
	} catch {
		session.login = null;
	}
}
