import type { Connector, ConnectorItem, ConnectorLogin, ConnectorsStatus } from '../src/lib/api.ts';
import { MockError } from './store.ts';

export const MOCK_CODE = '123456';
const PROMPT = 'Enter the 6-digit code sent to a***@example.com';
const HOUR = 3_600_000;

/** Two connectors: a bank that runs every morning, and a payroll site that waits for a sign-in with a code. */
export class MockConnectors {
	private connectors: Connector[] = [];
	private items: ConnectorItem[] = [];
	private nextId = 100;

	constructor(private readonly now: () => number = Date.now) {
		this.reset();
	}

	reset() {
		const at = (hours: number) => new Date(this.now() + hours * HOUR).toISOString();
		this.connectors = [
			{
				name: 'bank',
				kind: 'sgcb',
				schedule: 'daily 07:00',
				next_run: at(14),
				running: false,
				last_run: at(-10),
				last_success: at(-34),
				last_error: 'Relevé de novembre: the download timed out',
				last_report: {
					listed: 14,
					ingested: 0,
					skipped: 0,
					failed: 1,
					errors: ['Relevé de novembre: the download timed out'],
				},
				auth: 'ready',
				prompt: null,
				counts: { ingested: 12, skipped: 1, failed: 1 },
			},
			{
				name: 'mf',
				kind: 'moneyforward',
				schedule: 'monthly 26 09:00',
				next_run: null,
				running: false,
				last_run: at(-2),
				last_success: null,
				last_error: 'the site asks for a code; connect from the settings',
				last_report: null,
				auth: 'needs_login',
				prompt: null,
				counts: { ingested: 0, skipped: 0, failed: 0 },
			},
		];
		const item = (
			id: number,
			remote_id: string,
			title: string,
			status: ConnectorItem['status'],
			hours: number,
			extra: Partial<ConnectorItem> = {},
		): ConnectorItem => ({
			id,
			connector: 'bank',
			remote_id,
			sha256: status === 'ingested' ? id.toString(16).padStart(64, '0') : null,
			title,
			date: `${remote_id}-30`,
			status,
			detail: null,
			error: null,
			fetched_at: at(hours),
			...extra,
		});
		this.items = [
			item(4, '2026-11', 'Relevé de novembre', 'failed', -10, {
				error: 'the download timed out',
			}),
			item(3, '2026-10', "Relevé d'octobre", 'ingested', -34),
			item(2, '2026-09', 'Relevé de septembre', 'ingested', -34),
			item(1, '2026-08', 'Avis sans document', 'skipped', -34, { detail: 'empty file' }),
		];
	}

	private find(name: string): Connector {
		const connector = this.connectors.find((c) => c.name === name);
		if (!connector) throw new MockError(404, `not found: connector ${name}`);
		return connector;
	}

	status(): ConnectorsStatus {
		return structuredClone({
			enabled: true,
			utc_offset: '+09:00',
			connectors: this.connectors,
			items: this.items,
		});
	}

	run(name: string): { name: string; started: boolean } {
		const connector = this.find(name);
		const at = new Date(this.now()).toISOString();
		connector.last_run = at;
		if (connector.auth === 'ready') {
			connector.last_success = at;
			connector.last_error = null;
			connector.last_report = { listed: 14, ingested: 1, skipped: 0, failed: 0, errors: [] };
			connector.next_run = new Date(this.now() + 24 * HOUR).toISOString();
			const failed = this.items.find((i) => i.connector === name && i.status === 'failed');
			if (failed) {
				failed.status = 'ingested';
				failed.error = null;
				failed.fetched_at = at;
				connector.counts.failed -= 1;
				connector.counts.ingested += 1;
			} else {
				connector.counts.ingested += 1;
				this.items.unshift({
					id: this.nextId++,
					connector: name,
					remote_id: `doc-${this.nextId}`,
					sha256: this.nextId.toString(16).padStart(64, '0'),
					title: 'New document',
					date: at.slice(0, 10),
					status: 'ingested',
					detail: null,
					error: null,
					fetched_at: at,
				});
			}
		}
		return { name, started: true };
	}

	login(name: string): ConnectorLogin {
		const connector = this.find(name);
		if (connector.auth === 'ready') return { name, auth: 'ready', prompt: null };
		connector.auth = 'needs_code';
		connector.prompt = PROMPT;
		connector.next_run = null;
		return { name, auth: 'needs_code', prompt: PROMPT };
	}

	code(name: string, code: unknown): ConnectorLogin {
		const connector = this.find(name);
		if (connector.auth !== 'needs_code') {
			throw new MockError(409, 'no sign-in is waiting for a code; connect again');
		}
		if (code !== MOCK_CODE) throw new MockError(400, 'the code was not accepted: wrong code');
		connector.auth = 'ready';
		connector.prompt = null;
		this.run(name);
		return { name, auth: 'ready', prompt: null };
	}

	refetch(id: number): ConnectorItem {
		const at = this.items.findIndex((i) => i.id === id);
		const [item] = at < 0 ? [] : this.items.splice(at, 1);
		if (!item) throw new MockError(404, `not found: connector item ${id}`);
		const connector = this.connectors.find((c) => c.name === item.connector);
		if (connector) connector.counts[item.status] -= 1;
		return item;
	}
}
