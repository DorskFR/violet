/**
 * Page groups for a split. A cut `n` separates page `n` from page `n + 1`; cuts outside
 * `1..pageCount-1` are ignored.
 */
export function splitGroups(pageCount: number, cuts: Iterable<number>): number[][] {
	if (pageCount < 1) return [];
	const valid = new Set<number>();
	for (const cut of cuts) {
		if (Number.isInteger(cut) && cut >= 1 && cut < pageCount) valid.add(cut);
	}
	const groups: number[][] = [];
	let current: number[] = [];
	for (let page = 1; page <= pageCount; page += 1) {
		current.push(page);
		if (valid.has(page)) {
			groups.push(current);
			current = [];
		}
	}
	groups.push(current);
	return groups;
}

export function toggleCut(cuts: readonly number[], cut: number): number[] {
	return cuts.includes(cut) ? cuts.filter((c) => c !== cut) : [...cuts, cut].sort((a, b) => a - b);
}

export function describeGroup(group: readonly number[]): string {
	const first = group[0];
	const last = group[group.length - 1];
	if (first === undefined || last === undefined) return '';
	return first === last ? `Page ${first}` : `Pages ${first}–${last}`;
}
