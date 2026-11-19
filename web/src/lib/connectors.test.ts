import { describe, expect, it } from 'vitest';
import { runSummary, scheduleText } from './connectors.ts';

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
