import { ApiError, type Me } from './api';

/** A same-origin path to go back to after signing in; anything else goes to the search. */
export function safeNext(next: string | null | undefined): string {
	if (!next?.startsWith('/') || next.startsWith('//')) return '/';
	if ([...next].some((c) => c.charCodeAt(0) < 32 || c === '\\')) return '/';
	const url = new URL(next, 'http://violet.invalid');
	if (url.origin !== 'http://violet.invalid' || url.pathname === '/login') return '/';
	const path = `${url.pathname}${url.search}${url.hash}`;
	return path.startsWith('//') ? '/' : path;
}

export function loginHref(current: string): string {
	const next = safeNext(current);
	return next === '/' ? '/login' : `/login?next=${encodeURIComponent(next)}`;
}

/** The same rules as the server: 12 to 1024 bytes, not the user name. */
export function passwordProblem(password: string, name: string, confirm?: string): string | null {
	const bytes = new TextEncoder().encode(password).length;
	if (bytes < 12) return 'A password needs at least 12 characters.';
	if (bytes > 1024) return 'A password can be at most 1024 bytes long.';
	if (password === name.trim()) return 'The password cannot be the user name.';
	if (confirm !== undefined && confirm !== password) return 'The two passwords differ.';
	return null;
}

export function waitMessage(seconds: number | null): string {
	if (seconds === null) return 'Too many attempts. Wait a while before trying again.';
	if (seconds < 90) return `Too many attempts. Try again in ${Math.max(1, seconds)} seconds.`;
	return `Too many attempts. Try again in ${Math.ceil(seconds / 60)} minutes.`;
}

export function signInError(e: unknown, fallback: string): string {
	if (e instanceof ApiError && e.status === 429) return waitMessage(e.retryAfter);
	return e instanceof Error && e.message ? e.message : fallback;
}

export function hasSession(me: Me | null): boolean {
	return me?.via === 'session';
}
