import type { DuplicateCopy, DuplicatePair, Hit } from './api';

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

export type Field = 'title' | 'area' | 'issuer' | 'date' | 'pages' | 'size';

/** The fields whose values tell the two sides of a pair apart. */
export function differences(a: DuplicateCopy, b: DuplicateCopy): Field[] {
	const fields: Field[] = ['title', 'area', 'issuer', 'date', 'pages', 'size'];
	return fields.filter((field) => a[field] !== b[field]);
}

export function sameFile([a, b]: DuplicatePair): boolean {
	return a.sha256 === b.sha256;
}

/** Where a copy came from: the mail or the connector it was taken from, else the name of the file filed. */
export function origins(copy: Pick<DuplicateCopy, 'sources'>): string[] {
	const seen = new Set<string>();
	for (const { original_name, mail, connector } of copy.sources) {
		if (mail) seen.add(`Mail to ${mail.account}: ${mail.subject?.trim() || 'no subject'}`);
		else if (connector)
			seen.add(`Fetched by ${connector.name}: ${connector.title?.trim() || original_name}`);
		else seen.add(original_name);
	}
	return [...seen];
}
