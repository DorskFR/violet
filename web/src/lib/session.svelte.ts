import { type Me, rememberCsrf } from './api';

export const session = $state<{ me: Me | null }>({ me: null });

export function signedInAs(me: Me | null): void {
	rememberCsrf(me);
	session.me = me;
}
