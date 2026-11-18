import type { Me, PrivateArea, TokenInfo } from './api';

export function isAdmin(me: Me | null): boolean {
	return me !== null && me.role === 'admin' && me.scope === 'admin';
}

/** Identities typed one per line or separated by commas, trimmed, blanks and repeats dropped. */
export function parseIdentities(text: string): string[] {
	const out: string[] = [];
	for (const part of text.split(/[\n,]/)) {
		const value = part.trim();
		if (value && !out.includes(value)) out.push(value);
	}
	return out;
}

/** `YYYY-MM-DD` from a date input to the end of that local day as RFC 3339; blank means no expiry. */
export function expiryFromDate(date: string): string | undefined {
	const parts = /^(\d{4})-(\d{2})-(\d{2})$/.exec(date);
	if (!parts) return undefined;
	const [y, m, d] = [Number(parts[1]), Number(parts[2]), Number(parts[3])];
	const end = new Date(y, m - 1, d, 23, 59, 59);
	if (end.getFullYear() !== y || end.getMonth() !== m - 1 || end.getDate() !== d) return undefined;
	return end.toISOString();
}

export function isExpired(token: TokenInfo, now: Date = new Date()): boolean {
	return token.expires !== null && new Date(token.expires).getTime() <= now.getTime();
}

/** Empty area names and duplicate areas or members are dropped; the first occurrence wins. */
export function cleanAreas(areas: PrivateArea[]): PrivateArea[] {
	const out: PrivateArea[] = [];
	for (const { area, members } of areas) {
		const name = area.trim();
		if (!name || out.some((a) => a.area === name)) continue;
		out.push({ area: name, members: [...new Set(members.map((m) => m.trim()).filter(Boolean))] });
	}
	return out;
}
