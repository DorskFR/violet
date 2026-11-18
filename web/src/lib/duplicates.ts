import type { DuplicatePair, Hit } from './api';

export function pairKey([a, b]: DuplicatePair): string {
	return `${a.id}:${b.id}`;
}

/** The pairs left once `a` and `b` are said not to be copies of one another. */
export function withoutPair(pairs: DuplicatePair[], a: string, b: string): DuplicatePair[] {
	return pairs.filter(([x, y]) => !((x.id === a && y.id === b) || (x.id === b && y.id === a)));
}

/** The pairs left once the document `id` has left the library. */
export function withoutDocument(pairs: DuplicatePair[], id: string): DuplicatePair[] {
	return pairs.filter(([x, y]) => x.id !== id && y.id !== id);
}

/** How many different documents the pairs hold. */
export function documentCount(pairs: DuplicatePair[]): number {
	return new Set(pairs.flatMap(([a, b]) => [a.id, b.id])).size;
}

/** Only PDFs have pages to show. */
export function hasPages(hit: Pick<Hit, 'path'>): boolean {
	return hit.path.toLowerCase().endsWith('.pdf');
}
