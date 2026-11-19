import { describe, expect, it } from 'vitest';
import { ApiError } from './api';
import { loginHref, passwordProblem, safeNext, signInError, waitMessage } from './auth';

describe('safeNext', () => {
	it('keeps a path of this site with its query and hash', () => {
		expect(safeNext('/documents/abc?x=1#p2')).toBe('/documents/abc?x=1#p2');
		expect(safeNext('/?q=領収')).toBe('/?q=%E9%A0%98%E5%8F%8E');
	});

	it('refuses anything that could leave the site', () => {
		for (const next of [
			null,
			'',
			'https://evil.example/',
			'//evil.example/x',
			'/\\evil.example',
			'/\t/evil.example',
			'/a/..//evil.example',
			'javascript:alert(1)',
			'documents/abc',
		]) {
			expect(safeNext(next), String(next)).toBe('/');
		}
	});

	it('never returns to the sign-in page itself', () => {
		expect(safeNext('/login?next=/x')).toBe('/');
	});
});

describe('loginHref', () => {
	it('carries the page to come back to', () => {
		expect(loginHref('/documents/a b?x=1')).toBe('/login?next=%2Fdocuments%2Fa%2520b%3Fx%3D1');
		expect(loginHref('/')).toBe('/login');
		expect(loginHref('/login')).toBe('/login');
	});
});

describe('passwordProblem', () => {
	it('follows the server rules', () => {
		expect(passwordProblem('short', 'alex')).toMatch(/at least 12/);
		expect(passwordProblem('x'.repeat(1025), 'alex')).toMatch(/1024/);
		expect(passwordProblem('alexandria-12', 'alexandria-12 ')).toMatch(/user name/);
		expect(passwordProblem('long enough pass', 'alex', 'other')).toMatch(/differ/);
		expect(passwordProblem('long enough pass', 'alex', 'long enough pass')).toBeNull();
		expect(passwordProblem('long enough pass', 'alex')).toBeNull();
	});

	it('counts bytes, as the server does', () => {
		expect(passwordProblem('領収書領収', 'alex')).toBeNull();
		expect(passwordProblem('領収書', 'alex')).toMatch(/at least 12/);
	});
});

describe('waitMessage', () => {
	it('says how long to wait', () => {
		expect(waitMessage(900)).toBe('Too many attempts. Try again in 15 minutes.');
		expect(waitMessage(30)).toBe('Too many attempts. Try again in 30 seconds.');
		expect(waitMessage(null)).toMatch(/Wait a while/);
		expect(signInError(new ApiError(429, 'too many', 120), 'f')).toBe(
			'Too many attempts. Try again in 2 minutes.',
		);
		expect(signInError(new ApiError(401, 'wrong name or password'), 'f')).toBe(
			'wrong name or password',
		);
	});
});
