import type {
	Connector,
	ConnectorInput,
	ConnectorItem,
	ConnectorKind,
	ConnectorLogin,
	ConnectorOption,
	ConnectorsStatus,
	MailStatus,
	Me,
	ProcessedMail,
} from '../src/lib/api.ts';
import { MockError } from './store.ts';

export const MOCK_CODE = '123456';
const PROMPT = 'Enter the 6-digit code sent to a***@example.com';
const HOUR = 3_600_000;

const text = (
	name: string,
	label: string,
	extra: { default?: string; required?: boolean; placeholder?: string; help?: string } = {},
): ConnectorOption => ({
	name,
	label,
	help: extra.help ?? null,
	type: 'text',
	default: extra.default ?? null,
	required: extra.required ?? false,
	placeholder: extra.placeholder ?? null,
});

const RULE: ConnectorOption[] = [
	text('folder', 'Folder', { default: 'INBOX', required: true }),
	text('from', 'From contains', { placeholder: 'billing@example.com' }),
	text('to', 'To contains'),
	text('subject', 'Subject contains'),
	{
		name: 'filename_include',
		label: 'Only attachments named',
		help: 'Patterns with * and ?, such as *.pdf; none takes every attachment.',
		type: 'text_list',
	},
	{ name: 'filename_exclude', label: 'Never attachments named', help: null, type: 'text_list' },
	{
		name: 'max_age_days',
		label: 'Not older than, in days',
		help: null,
		type: 'number',
		default: 30,
		min: 1,
		max: null,
	},
	{
		name: 'scope',
		label: 'Take',
		help: null,
		type: 'choice',
		choices: [
			{ value: 'attachments', label: 'Attachments' },
			{ value: 'eml', label: 'The whole mail' },
			{ value: 'both', label: 'Both' },
		],
		default: 'attachments',
	},
	{
		name: 'include_inline',
		label: 'Also take inline images',
		help: null,
		type: 'boolean',
		default: false,
	},
	text('action', 'Afterwards', {
		default: 'keyword',
		required: true,
		help: 'What marks a taken mail on the server: keyword, keyword:<name>, seen, flag or move:<folder>.',
	}),
];

const KINDS: ConnectorKind[] = [
	{
		kind: 'moneyforward',
		label: 'Money Forward Cloud',
		signs_in: true,
		schedule: 'daily 09:00',
		options: [
			{
				name: 'documents',
				label: 'Documents',
				help: null,
				type: 'choices',
				choices: [
					{ value: 'payslips', label: 'Payslips' },
					{ value: 'bonus_slips', label: 'Bonus slips' },
					{ value: 'withholding_slips', label: 'Withholding slips' },
				],
				default: ['payslips', 'bonus_slips', 'withholding_slips'],
			},
			{
				name: 'since',
				label: 'Since',
				help: 'Older documents are left on the site; empty fetches everything.',
				type: 'date',
			},
		],
	},
	{
		kind: 'imap',
		label: 'Mailbox (IMAP)',
		signs_in: false,
		schedule: 'every 10m',
		options: [
			text('host', 'Host', { required: true, placeholder: 'imap.example.com' }),
			{
				name: 'port',
				label: 'Port',
				help: 'Empty uses 993 with TLS and 143 otherwise.',
				type: 'number',
				default: null,
				min: 1,
				max: 65535,
			},
			{
				name: 'security',
				label: 'Security',
				help: 'Without encryption only a mail server on this machine is accepted.',
				type: 'choice',
				choices: [
					{ value: 'tls', label: 'TLS' },
					{ value: 'starttls', label: 'STARTTLS' },
					{ value: 'plain', label: 'None' },
				],
				default: 'tls',
			},
			{
				name: 'rules',
				label: 'Rules',
				help: 'Each rule takes the matching mails of one folder.',
				type: 'group',
				item: 'Rule',
				fields: RULE,
				min: 1,
			},
		],
	},
	{ kind: 'sgcb', label: 'SG bank', signs_in: true, schedule: 'daily 09:00', options: [] },
];
const signsIn = (kind: string) => KINDS.find((k) => k.kind === kind)?.signs_in ?? true;
const SCHEDULE =
	/^(every [1-9]\d*[mhd]|daily \d{2}:\d{2}|weekly [a-z]{3} \d{2}:\d{2}|monthly ([1-9]|1\d|2[0-8]) \d{2}:\d{2})$/;

const isAdmin = (me: Me) => me.role === 'admin' && me.scope === 'admin';

/**
 * Three connectors: a bank that runs every morning, a payroll site that waits for a sign-in with
 * a code, and a mailbox with the mails it took.
 */
export class MockConnectors {
	private connectors: Connector[] = [];
	private items: ConnectorItem[] = [];
	private mails: ProcessedMail[] = [];
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
				owner: 'alex',
				username: 'alex@example.com',
				password_set: true,
				schedule: 'daily 07:00',
				utc_offset: '+09:00',
				area: 'Finance',
				tags: ['bank'],
				options: {},
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
				owner: 'sam',
				username: 'sam@example.com',
				password_set: true,
				schedule: 'monthly 26 09:00',
				utc_offset: '+09:00',
				area: null,
				tags: ['payroll'],
				options: { documents: ['payslips', 'bonus_slips'] },
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
			{
				name: 'home',
				kind: 'imap',
				owner: 'alex',
				username: 'alex@example.com',
				password_set: true,
				schedule: 'every 10m',
				utc_offset: '+09:00',
				area: null,
				tags: ['mail'],
				options: {
					host: 'imap.example.com',
					security: 'tls',
					rules: [{ folder: 'INBOX', filename_include: ['*.pdf'], max_age_days: 30 }],
				},
				next_run: at(0.1),
				running: false,
				last_run: at(-0.1),
				last_success: at(-0.1),
				last_error: null,
				last_report: { listed: 0, ingested: 0, skipped: 0, failed: 0, errors: [] },
				auth: 'ready',
				prompt: null,
				counts: { ingested: 1, skipped: 1, failed: 0 },
			},
		];
		const mail = (
			id: number,
			subject: string,
			sender: string,
			status: ProcessedMail['status'],
			detail: string | null,
		): ProcessedMail => ({
			id,
			account: 'home',
			folder: 'INBOX',
			uidvalidity: 1000,
			uid: id,
			message_id: `${id}@mail.example`,
			sender,
			subject,
			received: null,
			status,
			files: status === 'ingested' ? 1 : 0,
			detail,
			error: null,
			processed_at: at(-id),
		});
		this.mails = [
			mail(1, 'Your November invoice', 'Tokyo Gas <billing@gas.example>', 'ingested', null),
			mail(2, 'Your order 123', 'shop@example.com', 'skipped', 'no attachment'),
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

	private find(name: string, me?: Me): Connector {
		const connector = this.connectors.find((c) => c.name === name);
		if (!connector || (me && !isAdmin(me) && connector.owner !== me.name)) {
			throw new MockError(404, `not found: connector ${name}`);
		}
		return connector;
	}

	/** Everything for an admin and for a library without users; otherwise what `me` added. */
	status(me?: Me): ConnectorsStatus {
		const all = !me || isAdmin(me);
		const connectors = this.connectors.filter((c) => all || c.owner === me.name);
		const names = connectors.map((c) => c.name);
		return structuredClone({
			connectors,
			items: this.items.filter((i) => all || names.includes(i.connector)),
		});
	}

	kinds(): { kinds: ConnectorKind[] } {
		return structuredClone({ kinds: KINDS });
	}

	private checked(input: ConnectorInput, kind: string) {
		if (!KINDS.some((k) => k.kind === kind)) {
			throw new MockError(
				400,
				`unknown kind "${kind}" (known: ${KINDS.map((k) => k.kind).join(', ')})`,
			);
		}
		if (kind === 'imap' && !String(input.options?.host ?? '').trim()) {
			throw new MockError(400, 'options: enter the host of the mail server');
		}
		if (!String(input.username ?? '').trim()) {
			throw new MockError(400, 'enter the username the site signs in with');
		}
		if (!SCHEDULE.test(input.schedule)) {
			throw new MockError(400, `unknown schedule "${input.schedule}"`);
		}
		if (!/^[+-]\d{2}:\d{2}$/.test(input.utc_offset)) {
			throw new MockError(400, `"${input.utc_offset}" is not a UTC offset such as +09:00`);
		}
	}

	/** Whose a connector is: `kept` unless an admin names another user. */
	private owner(input: ConnectorInput, kept: string | null, me?: Me): string | null {
		if (!input.owner || input.owner === kept) return kept;
		if (me && !isAdmin(me)) {
			throw new MockError(403, 'only an admin chooses the owner of a connector');
		}
		return input.owner;
	}

	/** A mailbox signs in by itself and runs at once; a site waits for a sign-in. */
	create(input: ConnectorInput, me?: Me): Connector {
		const name = input.name ?? '';
		if (!/^[A-Za-z0-9._-]+$/.test(name)) {
			throw new MockError(400, "the name must be letters, digits, '-', '_' or '.'");
		}
		this.checked(input, input.kind ?? '');
		if (!input.password) throw new MockError(400, 'enter the password the site signs in with');
		if (this.connectors.some((c) => c.name === name)) {
			throw new MockError(409, `a connector named ${name} exists already`);
		}
		const waits = signsIn(input.kind ?? '');
		const connector: Connector = {
			name,
			kind: input.kind ?? '',
			owner: this.owner(input, me?.name ?? null, me),
			username: input.username.trim(),
			password_set: true,
			schedule: input.schedule,
			utc_offset: input.utc_offset,
			area: input.area,
			tags: input.tags,
			options: input.options,
			next_run: waits ? null : new Date(this.now()).toISOString(),
			running: false,
			last_run: null,
			last_success: null,
			last_error: null,
			last_report: null,
			auth: waits ? 'needs_login' : 'ready',
			prompt: null,
			counts: { ingested: 0, skipped: 0, failed: 0 },
		};
		this.connectors.push(connector);
		return structuredClone(connector);
	}

	/** A new username or password signs the connector of a site out. */
	replace(name: string, input: ConnectorInput, me?: Me): Connector {
		const connector = this.find(name, me);
		this.checked(input, connector.kind);
		const owner = this.owner(input, connector.owner, me);
		const username = input.username.trim();
		if (signsIn(connector.kind) && (input.password || username !== connector.username)) {
			connector.auth = 'needs_login';
			connector.prompt = null;
			connector.next_run = null;
		}
		Object.assign(connector, {
			owner,
			username,
			schedule: input.schedule,
			utc_offset: input.utc_offset,
			area: input.area,
			tags: input.tags,
			options: input.options,
		});
		return structuredClone(connector);
	}

	/** The documents it fetched stay listed. */
	remove(name: string, me?: Me): { name: string; removed: boolean } {
		const connector = this.find(name, me);
		this.connectors = this.connectors.filter((c) => c !== connector);
		return { name, removed: true };
	}

	run(name: string, me?: Me): { name: string; started: boolean } {
		const connector = this.find(name, me);
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

	login(name: string, me?: Me): ConnectorLogin {
		const connector = this.find(name, me);
		if (!signsIn(connector.kind)) {
			throw new MockError(
				400,
				'a connector of this kind signs in by itself at each run; there is nothing to connect',
			);
		}
		if (connector.auth === 'ready') return { name, auth: 'ready', prompt: null };
		connector.auth = 'needs_code';
		connector.prompt = PROMPT;
		connector.next_run = null;
		return { name, auth: 'needs_code', prompt: PROMPT };
	}

	code(name: string, code: unknown, me?: Me): ConnectorLogin {
		const connector = this.find(name, me);
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

	/** The mails of every mailbox for an admin; otherwise of the mailboxes of `me`. */
	mail(me?: Me): MailStatus {
		const names = this.status(me).connectors.map((c) => c.name);
		const all = !me || isAdmin(me);
		return structuredClone({
			processed: this.mails.filter((m) => all || names.includes(m.account)),
		});
	}

	reprocess(id: number, me?: Me): ProcessedMail {
		const at = this.mail(me).processed.some((m) => m.id === id)
			? this.mails.findIndex((m) => m.id === id)
			: -1;
		const [mail] = at < 0 ? [] : this.mails.splice(at, 1);
		if (!mail) throw new MockError(404, `not found: processed mail ${id}`);
		const connector = this.connectors.find((c) => c.name === mail.account);
		if (connector) connector.counts[mail.status] -= 1;
		return mail;
	}
}
