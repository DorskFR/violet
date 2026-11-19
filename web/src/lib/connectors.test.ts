import { describe, expect, it } from 'vitest';
import type { ConnectorKind, ConnectorOption } from './api.ts';
import {
	offsetText,
	optionDefaults,
	optionsOf,
	optionsSent,
	optionValues,
	runSummary,
	scheduleOf,
	scheduleParts,
	scheduleText,
} from './connectors.ts';

describe('scheduleText', () => {
	it('says each schedule in words, with the offset of clock times', () => {
		expect(scheduleText('every 1h', '+09:00')).toBe('Every hour');
		expect(scheduleText('every 15m', '+09:00')).toBe('Every 15 minutes');
		expect(scheduleText('every 2d', null)).toBe('Every 2 days');
		expect(scheduleText('daily 07:05', '+09:00')).toBe('Every day at 07:05 UTC+09:00');
		expect(scheduleText('daily 07:05', '+00:00')).toBe('Every day at 07:05 UTC');
		expect(scheduleText('weekly sun 23:59', '-03:30')).toBe('Every Sunday at 23:59 UTC-03:30');
		expect(scheduleText('monthly 26 09:00', '+09:00')).toBe(
			'On the 26th of each month at 09:00 UTC+09:00',
		);
		expect(scheduleText('monthly 1 09:00', null)).toBe('On the 1st of each month at 09:00 UTC');
		expect(scheduleText('monthly 22 09:00', null)).toMatch(/^On the 22nd /);
		expect(scheduleText('monthly 3 09:00', null)).toMatch(/^On the 3rd /);
		expect(scheduleText('monthly 12 09:00', null)).toMatch(/^On the 12th /);
	});

	it('shows a schedule it does not know as it is', () => {
		expect(scheduleText('hourly', '+09:00')).toBe('hourly');
		expect(scheduleText('weekly someday 09:00', null)).toBe('weekly someday 09:00');
	});
});

describe('runSummary', () => {
	const report = { listed: 3, ingested: 2, skipped: 0, failed: 0, errors: [] };

	it('counts what was fetched and what the last run brought', () => {
		expect(
			runSummary({ counts: { ingested: 12, skipped: 0, failed: 0 }, last_report: report }),
		).toBe('12 fetched, 2 new last run');
		expect(
			runSummary({
				counts: { ingested: 12, skipped: 1, failed: 2 },
				last_report: { ...report, ingested: 0 },
			}),
		).toBe('12 fetched, 1 skipped, 2 failed, nothing new last run');
		expect(runSummary({ counts: { ingested: 0, skipped: 0, failed: 0 }, last_report: null })).toBe(
			'0 fetched',
		);
	});
});

describe('scheduleParts', () => {
	it('reads each schedule into the form and writes it back', () => {
		for (const schedule of [
			'every 15m',
			'every 2d',
			'daily 07:05',
			'weekly sun 23:59',
			'monthly 26 09:00',
		]) {
			expect(scheduleOf(scheduleParts(schedule))).toBe(schedule);
		}
		expect(scheduleParts('weekly sun 23:59')).toMatchObject({ every: 'weekly', weekday: 'sun' });
		expect(scheduleParts('monthly 26 09:00')).toMatchObject({ every: 'monthly', monthDay: '26' });
	});

	it('starts from every day at nine for a schedule it does not know', () => {
		expect(scheduleOf(scheduleParts(''))).toBe('daily 09:00');
		expect(scheduleOf(scheduleParts('weekly someday 09:00'))).toBe('daily 09:00');
	});

	it('drops the seconds a time field may add', () => {
		expect(scheduleOf({ ...scheduleParts('daily 07:05'), at: '07:05:00' })).toBe('daily 07:05');
	});
});

describe('offsetText', () => {
	it('writes the offset of the browser the way the server reads it', () => {
		expect(offsetText(-540)).toBe('+09:00');
		expect(offsetText(210)).toBe('-03:30');
		expect(offsetText(0)).toBe('+00:00');
	});
});

describe('options of a kind', () => {
	const kind: ConnectorKind = {
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
				],
				default: ['payslips', 'bonus_slips'],
			},
			{ name: 'since', label: 'Since', help: null, type: 'date' },
		],
	};

	it('start from the defaults of the kind', () => {
		expect(optionDefaults(kind)).toEqual({ documents: ['payslips', 'bonus_slips'], since: '' });
		expect(optionDefaults(undefined)).toEqual({});
	});

	it('are sent in the order of the kind, without an empty date or what the kind does not know', () => {
		expect(optionsOf(kind, { documents: ['bonus_slips', 'payslips'], since: ' ', old: 1 })).toEqual(
			{
				documents: ['payslips', 'bonus_slips'],
			},
		);
		expect(optionsOf(kind, { documents: [], since: '2026-01-01' })).toEqual({
			documents: [],
			since: '2026-01-01',
		});
		expect(optionsOf(undefined, { company: 'acme' })).toEqual({});
	});
});

describe('options of a mailbox', () => {
	const options: ConnectorOption[] = [
		{
			name: 'host',
			label: 'Host',
			help: null,
			type: 'text',
			default: null,
			required: true,
			placeholder: 'imap.example.com',
		},
		{ name: 'port', label: 'Port', help: null, type: 'number', default: null, min: 1, max: 65535 },
		{
			name: 'security',
			label: 'Security',
			help: null,
			type: 'choice',
			choices: [
				{ value: 'tls', label: 'TLS' },
				{ value: 'plain', label: 'None' },
			],
			default: 'tls',
		},
		{
			name: 'rules',
			label: 'Rules',
			help: null,
			type: 'group',
			item: 'Rule',
			min: 1,
			fields: [
				{
					name: 'folder',
					label: 'Folder',
					help: null,
					type: 'text',
					default: 'INBOX',
					required: true,
					placeholder: null,
				},
				{ name: 'filename_include', label: 'Only', help: null, type: 'text_list' },
				{
					name: 'max_age_days',
					label: 'Days',
					help: null,
					type: 'number',
					default: 30,
					min: 1,
					max: null,
				},
				{ name: 'include_inline', label: 'Inline', help: null, type: 'boolean', default: false },
			],
		},
	];
	const rule = { folder: 'INBOX', filename_include: '', max_age_days: '30', include_inline: false };

	it('start from one rule with the defaults', () => {
		expect(optionValues(options)).toEqual({ host: '', port: '', security: 'tls', rules: [rule] });
	});

	it('are shown as they were stored', () => {
		const stored = {
			host: 'imap.example.com',
			port: 1143,
			security: 'plain',
			rules: [
				{ folder: 'Bills', filename_include: ['*.pdf', '*.jpg'], include_inline: true },
				{ max_age_days: 7 },
			],
		};
		expect(optionValues(options, stored)).toEqual({
			host: 'imap.example.com',
			port: '1143',
			security: 'plain',
			rules: [
				{ ...rule, folder: 'Bills', filename_include: '*.pdf, *.jpg', include_inline: true },
				{ ...rule, max_age_days: '7' },
			],
		});
	});

	it('are sent with numbers as numbers, lists as arrays and empty fields left out', () => {
		const values = {
			host: ' imap.example.com ',
			port: ' ',
			security: 'ssl',
			rules: [
				{ ...rule, filename_include: ' *.pdf, ,*.jpg ', max_age_days: 7, include_inline: true },
				{ ...rule, folder: '', max_age_days: 'soon' },
			],
		};
		expect(optionsSent(options, values)).toEqual({
			host: 'imap.example.com',
			security: 'tls',
			rules: [
				{
					folder: 'INBOX',
					filename_include: ['*.pdf', '*.jpg'],
					max_age_days: 7,
					include_inline: true,
				},
				{ filename_include: [], max_age_days: 'soon', include_inline: false },
			],
		});
		expect(optionsSent(options, optionValues(options))).toEqual({
			security: 'tls',
			rules: [{ folder: 'INBOX', filename_include: [], max_age_days: 30, include_inline: false }],
		});
	});
});
