export function currencyDigits(currency: string): number {
	try {
		return (
			new Intl.NumberFormat('en', { style: 'currency', currency }).resolvedOptions()
				.maximumFractionDigits ?? 2
		);
	} catch {
		return 2;
	}
}

export function formatAmount(
	minor: number | null | undefined,
	currency: string | null | undefined,
	locale?: string,
): string {
	if (minor === null || minor === undefined) return '';
	if (!currency) return new Intl.NumberFormat(locale).format(minor);
	const digits = currencyDigits(currency);
	const value = minor / 10 ** digits;
	try {
		return new Intl.NumberFormat(locale, { style: 'currency', currency }).format(value);
	} catch {
		return `${new Intl.NumberFormat(locale, { minimumFractionDigits: digits }).format(value)} ${currency}`;
	}
}

/** The amount as typed into a form field: major units, no grouping. */
export function amountToInput(
	minor: number | null | undefined,
	currency: string | null | undefined,
): string {
	if (minor === null || minor === undefined) return '';
	const digits = currency ? currencyDigits(currency) : 0;
	return (minor / 10 ** digits).toFixed(digits);
}

/** Major units as typed ("1,200", "12.50", "¥980") to minor units; null when empty or not a number. */
export function parseAmount(text: string, currency: string | null | undefined): number | null {
	const cleaned = text
		.normalize('NFKC')
		.replace(/[,\s_'¥$€£円]/g, '')
		.trim();
	if (!/^-?\d+(\.\d+)?$/.test(cleaned)) return null;
	const digits = currency ? currencyDigits(currency) : 0;
	return Math.round(Number(cleaned) * 10 ** digits);
}

const ISO_DATE = /^(\d{4})-(\d{2})-(\d{2})$/;

/** A `YYYY-MM-DD` document date for display; anything else is returned as is. */
export function formatDate(date: string | null | undefined, locale?: string): string {
	if (!date) return '';
	const m = ISO_DATE.exec(date);
	if (!m) return date;
	const utc = new Date(Date.UTC(Number(m[1]), Number(m[2]) - 1, Number(m[3])));
	if (Number.isNaN(utc.getTime())) return date;
	return new Intl.DateTimeFormat(locale, {
		year: 'numeric',
		month: 'short',
		day: 'numeric',
		timeZone: 'UTC',
	}).format(utc);
}

export function isIsoDate(text: string): boolean {
	const m = ISO_DATE.exec(text);
	if (!m) return false;
	const d = new Date(Date.UTC(Number(m[1]), Number(m[2]) - 1, Number(m[3])));
	return d.getUTCMonth() === Number(m[2]) - 1 && d.getUTCDate() === Number(m[3]);
}

export function formatBytes(bytes: number): string {
	if (bytes < 1024) return `${bytes} B`;
	const units = ['KB', 'MB', 'GB', 'TB'];
	let value = bytes / 1024;
	let unit = 0;
	while (value >= 1024 && unit < units.length - 1) {
		value /= 1024;
		unit += 1;
	}
	return `${value < 10 ? value.toFixed(1) : Math.round(value)} ${units[unit]}`;
}

export function pageCount(pages: number): string {
	return pages === 1 ? '1 page' : `${pages} pages`;
}

export function fileName(path: string): string {
	return path.split('/').pop() ?? path;
}

export function parseTags(text: string): string[] {
	const seen = new Set<string>();
	for (const raw of text.split(/[,、\n]/)) {
		const tag = raw.trim();
		if (tag) seen.add(tag);
	}
	return [...seen];
}

export const DEFAULT_CURRENCY = 'JPY';

/** Currencies to offer: the ones the library uses, most used first, then any `extra` in use. */
export function currencyChoices(
	facet: readonly { value: string }[] | undefined,
	...extra: string[]
): string[] {
	const known = facet?.length ? facet.map((f) => f.value) : [DEFAULT_CURRENCY];
	return [...new Set([...known, ...extra.map((c) => c.trim().toUpperCase()).filter(Boolean)])];
}
