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
});
