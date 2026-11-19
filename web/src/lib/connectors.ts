import type { Connector, ConnectorAuth } from './api.ts';

const DAYS: Record<string, string> = {
	mon: 'Monday',
	tue: 'Tuesday',
	wed: 'Wednesday',
	thu: 'Thursday',
	fri: 'Friday',
	sat: 'Saturday',
	sun: 'Sunday',
};
const UNITS: Record<string, string> = { m: 'minute', h: 'hour', d: 'day' };

function ordinal(day: number): string {
	const tens = day % 100;
	const suffix = tens >= 11 && tens <= 13 ? 'th' : (['th', 'st', 'nd', 'rd'][day % 10] ?? 'th');
	return `${day}${suffix}`;
}

/** A schedule of the connectors config in words; clock times are in `offset` (`+09:00`). */
export function scheduleText(schedule: string, offset: string | null): string {
	const [kind, first, second] = schedule.split(' ');
	const zone = !offset || offset === '+00:00' ? 'UTC' : `UTC${offset}`;
	const every = /^(\d+)([mhd])$/.exec(first ?? '');
	if (kind === 'every' && every) {
		const count = Number(every[1]);
		const unit = UNITS[every[2] ?? ''] ?? '';
		return count === 1 ? `Every ${unit}` : `Every ${count} ${unit}s`;
	}
	if (kind === 'daily' && first) return `Every day at ${first} ${zone}`;
	if (kind === 'weekly' && first && second && DAYS[first]) {
		return `Every ${DAYS[first]} at ${second} ${zone}`;
	}
	if (kind === 'monthly' && first && second && Number(first) > 0) {
		return `On the ${ordinal(Number(first))} of each month at ${second} ${zone}`;
	}
	return schedule;
}

export const AUTH_LABEL: Record<ConnectorAuth, string> = {
	ready: 'signed in',
	needs_code: 'waiting for a code',
	needs_login: 'sign-in needed',
};

/** What the last run did, in a few words. */
export function runSummary(connector: Pick<Connector, 'last_report' | 'counts'>): string {
	const { ingested, skipped, failed } = connector.counts;
	const parts = [`${ingested} fetched`];
	if (skipped) parts.push(`${skipped} skipped`);
	if (failed) parts.push(`${failed} failed`);
	const last = connector.last_report;
	if (last) parts.push(last.ingested ? `${last.ingested} new last run` : 'nothing new last run');
	return parts.join(', ');
}
