import { describe, expect, it } from 'vitest';
import { MOCK_CODE, MockConnectors } from './connectors.ts';
import { MockError } from './store.ts';

describe('MockConnectors', () => {
	it('signs a connector in with the code the site sent', () => {
		const mock = new MockConnectors(() => Date.UTC(2026, 10, 19));
		const before = mock.status().connectors.find((c) => c.name === 'mf');
		expect(before).toMatchObject({ auth: 'needs_login', next_run: null });
		expect(() => mock.code('mf', MOCK_CODE)).toThrow(MockError);
		expect(mock.login('mf')).toMatchObject({ auth: 'needs_code' });
		expect(mock.status().connectors[1]?.prompt).toContain('code');
		expect(() => mock.code('mf', '000000')).toThrow('wrong code');
		expect(mock.code('mf', MOCK_CODE).auth).toBe('ready');
		const after = mock.status().connectors[1];
		expect(after).toMatchObject({ auth: 'ready', last_error: null });
		expect(after?.next_run).not.toBeNull();
		expect(mock.status().items[0]?.connector).toBe('mf');
	});

	it('retries the failed document on a run and forgets a refetched one', () => {
		const mock = new MockConnectors(() => Date.UTC(2026, 10, 19));
		expect(mock.status().connectors[0]?.counts).toEqual({ ingested: 12, skipped: 1, failed: 1 });
		mock.run('bank');
		const bank = mock.status().connectors[0];
		expect(bank?.counts).toEqual({ ingested: 13, skipped: 1, failed: 0 });
		expect(bank?.last_error).toBeNull();
		expect(mock.refetch(1).status).toBe('skipped');
		expect(mock.status().items.map((i) => i.id)).toEqual([4, 3, 2]);
		expect(() => mock.refetch(1)).toThrow(MockError);
		expect(() => mock.run('nope')).toThrow(MockError);
	});

	const input = {
		name: 'payslips',
		kind: 'moneyforward',
		username: 'kim@example.com',
		password: 'mock-password',
		schedule: 'monthly 26 09:00',
		utc_offset: '+09:00',
		area: null,
		tags: [],
		options: { documents: ['payslips'] },
	};
	const me = (name: string, role: 'admin' | 'member') => ({
		csrf: 'c',
		name,
		role,
		scope: role === 'admin' ? ('admin' as const) : ('write' as const),
		via: 'session' as const,
		areas: [],
		users: true,
		password: true,
		passkeys: [],
	});

	it('adds, edits and removes a connector without ever showing its password', () => {
		const mock = new MockConnectors(() => Date.UTC(2026, 10, 19));
		expect(mock.kinds().kinds.map((k) => k.kind)).toContain('moneyforward');
		expect(() => mock.create({ ...input, schedule: 'sometimes' })).toThrow('unknown schedule');
		expect(() => mock.create({ ...input, password: null })).toThrow('enter the password');
		const added = mock.create(input);
		expect(added).toMatchObject({ name: 'payslips', owner: null, auth: 'needs_login' });
		expect(JSON.stringify(mock.status())).not.toContain('mock-password');
		expect(() => mock.create(input)).toThrow('exists already');

		mock.login('payslips');
		mock.code('payslips', MOCK_CODE);
		const kept = mock.replace('payslips', { ...input, password: null, schedule: 'daily 07:00' });
		expect(kept).toMatchObject({ auth: 'ready', schedule: 'daily 07:00' });
		const changed = mock.replace('payslips', { ...input, password: 'another' });
		expect(changed).toMatchObject({ auth: 'needs_login', next_run: null });

		expect(mock.remove('payslips')).toEqual({ name: 'payslips', removed: true });
		expect(() => mock.remove('payslips')).toThrow(MockError);
		expect(mock.status().items.some((i) => i.connector === 'payslips')).toBe(true);
	});

	it('shows a member the connectors they added and an admin all of them', () => {
		const mock = new MockConnectors(() => Date.UTC(2026, 10, 19));
		const [alex, sam, kim] = [me('alex', 'admin'), me('sam', 'member'), me('kim', 'member')];
		expect(mock.create(input, kim).owner).toBe('kim');
		expect(mock.status(alex).connectors.map((c) => c.name)).toEqual(['bank', 'mf', 'payslips']);
		expect(mock.status(sam).connectors.map((c) => c.name)).toEqual(['mf']);
		expect(mock.status(kim).connectors.map((c) => c.name)).toEqual(['payslips']);
		expect(mock.status(kim).items).toEqual([]);
		expect(() => mock.replace('mf', input, kim)).toThrow('not found');
		expect(() => mock.remove('bank', sam)).toThrow('not found');
		expect(() => mock.login('mf', kim)).toThrow('not found');
		expect(mock.replace('payslips', { ...input, password: null }, alex).owner).toBe('kim');
	});
});
