import type { DocumentPatch, Fact, Sidecar } from './api';
import { amountToInput, isIsoDate, parseAmount, parseTags } from './format';

export type FactDraft = { key: string; value: string; amount: string; currency: string };

export type Draft = {
	title: string;
	area: string;
	doc_type: string;
	issuer: string;
	date: string;
	tags: string;
	facts: FactDraft[];
};

export type DraftErrors = Partial<Record<'title' | 'area' | 'doc_type' | 'date', string>> & {
	facts?: Record<number, string>;
};

export function draftOf(sidecar: Sidecar): Draft {
	return {
		title: sidecar.title,
		area: sidecar.area,
		doc_type: sidecar.doc_type,
		issuer: sidecar.issuer ?? '',
		date: sidecar.date ?? '',
		tags: sidecar.tags.join(', '),
		facts: sidecar.facts.map((f) => ({
			key: f.key,
			value: f.value,
			amount: amountToInput(f.amount_minor, f.currency),
			currency: f.currency ?? '',
		})),
	};
}

const same = (a: unknown, b: unknown) => JSON.stringify(a) === JSON.stringify(b);

/** The fields of `draft` that differ from `sidecar`, or the reasons it cannot be saved. */
export function patchOf(
	sidecar: Sidecar,
	draft: Draft,
): { patch: DocumentPatch; errors: null } | { patch: null; errors: DraftErrors } {
	const errors: DraftErrors = {};
	const title = draft.title.trim();
	const area = draft.area.trim();
	const docType = draft.doc_type.trim();
	const date = draft.date.trim();
	if (!title) errors.title = 'A document needs a title.';
	if (!area) errors.area = 'Pick the area it is filed under.';
	else if (/[/\\]/.test(area)) errors.area = 'An area is one folder name, without slashes.';
	if (!docType) errors.doc_type = 'Say what kind of document this is.';
	if (date && !isIsoDate(date)) errors.date = 'Use a real date as YYYY-MM-DD, or leave it empty.';

	const facts: Fact[] = [];
	const factErrors: Record<number, string> = {};
	draft.facts.forEach((f, i) => {
		const key = f.key.trim();
		const value = f.value.trim();
		const amountText = f.amount.trim();
		const currency = f.currency.trim().toUpperCase();
		if (!key && !value && !amountText) return;
		if (!key) {
			factErrors[i] = 'Name this fact, for example "total".';
			return;
		}
		let amount: number | null = null;
		if (amountText) {
			if (!currency) {
				factErrors[i] = 'An amount needs a currency, for example JPY.';
				return;
			}
			amount = parseAmount(amountText, currency);
			if (amount === null) {
				factErrors[i] = 'The amount is not a number.';
				return;
			}
		}
		facts.push({
			key,
			value: value || amountText,
			amount_minor: amount,
			currency: amount === null ? null : currency,
		});
	});
	if (Object.keys(factErrors).length) errors.facts = factErrors;
	if (Object.keys(errors).length) return { patch: null, errors };

	const patch: DocumentPatch = {};
	if (title !== sidecar.title) patch.title = title;
	if (area !== sidecar.area) patch.area = area;
	if (docType !== sidecar.doc_type) patch.doc_type = docType;
	const issuer = draft.issuer.trim() || null;
	if (issuer !== sidecar.issuer) patch.issuer = issuer;
	if ((date || null) !== sidecar.date) patch.date = date || null;
	const tags = parseTags(draft.tags);
	if (!same(tags, sidecar.tags)) patch.tags = tags;
	if (!same(facts, sidecar.facts)) patch.facts = facts;
	return { patch, errors: null };
}

export function factLabel(key: string): string {
	const text = key.replace(/_/g, ' ').trim();
	return text ? text[0]?.toUpperCase() + text.slice(1) : key;
}
