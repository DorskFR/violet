import { describe, expect, it } from 'vitest';
import type { Me } from './api';
import { canWrite, cleanAreas, expiryFromDate, isAdmin, isExpired, parseNames } from './users';

const me = (over: Partial<Me>): Me => ({
	csrf: 'c',
	name: 'alex',
	role: 'admin',
	scope: 'admin',
	via: 'session',
	areas: [],
	users: true,
	password: true,
	passkeys: [],
	...over,
});

describe('canWrite', () => {
	it('is anyone signed in whose scope is not read-only', () => {
		expect(canWrite(me({ role: 'member', scope: 'write' }))).toBe(true);
		expect(canWrite(me({}))).toBe(true);
		expect(canWrite(me({ scope: 'read' }))).toBe(false);
		expect(canWrite(null)).toBe(false);
	});
});

describe('isAdmin', () => {
	it('needs the admin role and an admin scope', () => {
		expect(isAdmin(me({}))).toBe(true);
		expect(isAdmin(me({ scope: 'write' }))).toBe(false);
		expect(isAdmin(me({ role: 'member' }))).toBe(false);
		expect(isAdmin(null)).toBe(false);
	});

	it('treats the implicit user of an unconfigured library as admin', () => {
		expect(isAdmin(me({ csrf: null, name: null, via: 'implicit', users: false }))).toBe(true);
	});
});

describe('parseNames', () => {
	it('splits on commas and lines and drops blanks and repeats', () => {
		expect(parseNames(' sam, alex\n\nsam ,')).toEqual(['sam', 'alex']);
		expect(parseNames('   ')).toEqual([]);
	});
});

describe('expiryFromDate', () => {
	it('ends on the last second of the chosen local day', () => {
		const value = expiryFromDate('2027-03-31');
		expect(value).toBeDefined();
		const end = new Date(value as string);
		expect([end.getFullYear(), end.getMonth(), end.getDate()]).toEqual([2027, 2, 31]);
		expect([end.getHours(), end.getMinutes(), end.getSeconds()]).toEqual([23, 59, 59]);
		expect(value).toMatch(/^\d{4}-\d{2}-\d{2}T\d{2}:\d{2}:\d{2}\.\d{3}Z$/);
	});

	it('means no expiry for a blank or impossible date', () => {
		expect(expiryFromDate('')).toBeUndefined();
		expect(expiryFromDate('2027-02-30')).toBeUndefined();
		expect(expiryFromDate('31/03/2027')).toBeUndefined();
	});
});

describe('isExpired', () => {
	const now = new Date('2027-01-01T00:00:00Z');
	it('compares the expiry with now', () => {
		expect(isExpired({ name: 't', scope: 'read', expires: null }, now)).toBe(false);
		expect(isExpired({ name: 't', scope: 'read', expires: '2026-12-31T23:59:59Z' }, now)).toBe(
			true,
		);
		expect(isExpired({ name: 't', scope: 'read', expires: '2027-01-02T00:00:00Z' }, now)).toBe(
			false,
		);
	});
});

describe('cleanAreas', () => {
	it('trims names, drops empty and repeated areas and repeated members', () => {
		expect(
			cleanAreas([
				{ area: ' Health/Sam ', members: ['sam', 'sam', ' '] },
				{ area: '', members: ['alex'] },
				{ area: 'Health/Sam', members: ['alex'] },
				{ area: 'Tax', members: [] },
			]),
		).toEqual([
			{ area: 'Health/Sam', members: ['sam'] },
			{ area: 'Tax', members: [] },
		]);
	});
});
