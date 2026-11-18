import type { Action, DocRef } from './api';
import { fileName } from './format';

export type AffectedDocument = {
	id: string;
	label: string;
	/** The document no longer exists under this id once the action has run. */
	gone: boolean;
};

const ref = ([id, path]: DocRef, gone = false): AffectedDocument => ({
	id,
	label: fileName(path),
	gone,
});

export function affectedDocuments(action: Action): AffectedDocument[] {
	switch (action.kind) {
		case 'ingest':
			return action.documents.map((d) => ref(d));
		case 'duplicate':
			return [{ id: action.of, label: 'the existing copy', gone: false }];
		case 'split':
			return [ref(action.from, true), ...action.into.map((d) => ref(d))];
		case 'merge':
			return [...action.originals.map(([, id, path]) => ref([id, path], true)), ref(action.into)];
		case 'edit_pages':
			return [ref(action.document)];
		case 'update':
		case 'replace':
			return [ref([action.document, action.to])];
		case 'delete':
			return [ref(action.document, true)];
		case 'undo':
			return [];
	}
}

export const ACTION_LABEL: Record<Action['kind'], string> = {
	ingest: 'Filed',
	duplicate: 'Duplicate',
	split: 'Split',
	merge: 'Merged',
	edit_pages: 'Pages edited',
	update: 'Edited',
	delete: 'Deleted',
	replace: 'Replaced',
	undo: 'Undone',
};
