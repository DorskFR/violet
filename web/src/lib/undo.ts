export type UndoToast = {
	tone: 'ok';
	action?: { label: string; run: () => Promise<void> };
};

export type UndoDeps = {
	undo: (entry: string) => Promise<unknown>;
	show: (message: string, options: UndoToast) => unknown;
};

/**
 * Says a change was made and offers to undo it. `restored` runs once the undo went through,
 * to show the library as it is again. Without an entry nothing changed, so nothing is offered.
 */
export function announce(
	deps: UndoDeps,
	message: string,
	entry: string | null,
	restored: () => void | Promise<void>,
): void {
	if (!entry) {
		deps.show(message, { tone: 'ok' });
		return;
	}
	deps.show(message, {
		tone: 'ok',
		action: {
			label: 'Undo',
			run: async () => {
				await deps.undo(entry);
				await restored();
				deps.show('Undone', { tone: 'ok' });
			},
		},
	});
}
