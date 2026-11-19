import type { Connector, ConnectorAuth, ConnectorKind } from './api.ts';

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

/** A schedule of a connector in words; clock times are in `offset` (`+09:00`). */
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

/** A schedule as the form asks for it; `scheduleOf` writes it back. */
export type ScheduleParts = {
	every: 'interval' | 'daily' | 'weekly' | 'monthly';
	count: string;
	unit: string;
	weekday: string;
	monthDay: string;
	at: string;
};

export const FREQUENCIES = [
	{ value: 'interval', label: 'Every few minutes, hours or days' },
	{ value: 'daily', label: 'Every day' },
	{ value: 'weekly', label: 'Every week' },
	{ value: 'monthly', label: 'Every month' },
];
export const INTERVAL_UNITS = [
	{ value: 'm', label: 'minutes' },
	{ value: 'h', label: 'hours' },
	{ value: 'd', label: 'days' },
];
export const WEEKDAYS = Object.entries(DAYS).map(([value, label]) => ({ value, label }));

const CLOCK = /^\d{2}:\d{2}$/;

/** The parts of a schedule; one it does not know reads as every day at 09:00. */
export function scheduleParts(schedule: string): ScheduleParts {
	const parts: ScheduleParts = {
		every: 'daily',
		count: '6',
		unit: 'h',
		weekday: 'mon',
		monthDay: '1',
		at: '09:00',
	};
	const [kind, first, second] = schedule.trim().split(/\s+/);
	const interval = /^(\d+)([mhd])$/.exec(first ?? '');
	if (kind === 'every' && interval) {
		return { ...parts, every: 'interval', count: interval[1] ?? '', unit: interval[2] ?? 'h' };
	}
	if (kind === 'daily' && CLOCK.test(first ?? '')) return { ...parts, at: first ?? '' };
	if (kind === 'weekly' && first && DAYS[first] && CLOCK.test(second ?? '')) {
		return { ...parts, every: 'weekly', weekday: first, at: second ?? '' };
	}
	if (kind === 'monthly' && /^\d+$/.test(first ?? '') && CLOCK.test(second ?? '')) {
		return { ...parts, every: 'monthly', monthDay: first ?? '', at: second ?? '' };
	}
	return parts;
}

export function scheduleOf(parts: ScheduleParts): string {
	const at = parts.at.slice(0, 5);
	if (parts.every === 'interval') return `every ${String(parts.count).trim()}${parts.unit}`;
	if (parts.every === 'weekly') return `weekly ${parts.weekday} ${at}`;
	if (parts.every === 'monthly') return `monthly ${String(parts.monthDay).trim()} ${at}`;
	return `daily ${at}`;
}

/** `+09:00` for a zone `minutesBehind` UTC, as `Date.getTimezoneOffset` counts them (-540). */
export function offsetText(minutesBehind: number): string {
	const minutes = Math.abs(Math.round(minutesBehind));
	const two = (n: number) => String(n).padStart(2, '0');
	return `${minutesBehind > 0 ? '-' : '+'}${two(Math.floor(minutes / 60))}:${two(minutes % 60)}`;
}

/** What the form of a kind starts from: every choice its default, every date empty. */
export function optionDefaults(kind: ConnectorKind | undefined): Record<string, unknown> {
	const values: Record<string, unknown> = {};
	for (const option of kind?.options ?? []) {
		values[option.name] = option.type === 'choices' ? [...option.default] : '';
	}
	return values;
}

/** The options to send: those of the kind, chosen values in the kind's order, empty dates left out. */
export function optionsOf(
	kind: ConnectorKind | undefined,
	values: Record<string, unknown>,
): Record<string, unknown> {
	const options: Record<string, unknown> = {};
	for (const option of kind?.options ?? []) {
		const value = values[option.name];
		if (option.type === 'choices') {
			const chosen = Array.isArray(value) ? value : option.default;
			options[option.name] = option.choices.map((c) => c.value).filter((v) => chosen.includes(v));
		} else if (typeof value === 'string' && value.trim()) {
			options[option.name] = value.trim();
		}
	}
	return options;
}
