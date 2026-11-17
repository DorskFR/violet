import { describe, expect, it, vi } from 'vitest';
import { announce, type UndoToast } from './undo';

function setup(undo: (entry: string) => Promise<unknown> = async () => ({})) {
	const shown: { message: string; options: UndoToast }[] = [];
	const deps = {
		undo: vi.fn(undo),
		show: (message: string, options: UndoToast) => shown.push({ message, options }),
	};
	return { deps, shown };
}

describe('announce', () => {
	it('offers Undo for a change that wrote a journal entry', () => {
		const { deps, shown } = setup();
		announce(deps, 'Page 2 rotated', 'e1', () => {});
		expect(shown).toHaveLength(1);
		expect(shown[0]?.message).toBe('Page 2 rotated');
		expect(shown[0]?.options.action?.label).toBe('Undo');
		expect(deps.undo).not.toHaveBeenCalled();
	});

	it('offers nothing when the change wrote no entry', () => {
		const { deps, shown } = setup();
		const restored = vi.fn();
		announce(deps, 'Saved', null, restored);
		expect(shown).toEqual([{ message: 'Saved', options: { tone: 'ok' } }]);
		expect(restored).not.toHaveBeenCalled();
	});

	it('undoes the entry, then restores the view, then confirms', async () => {
		const order: string[] = [];
		const { deps, shown } = setup(async (entry) => {
			order.push(`undo ${entry}`);
		});
		announce(deps, 'Document deleted', 'e7', async () => {
			order.push('restored');
		});
		await shown[0]?.options.action?.run();
		expect(order).toEqual(['undo e7', 'restored']);
		expect(shown.map((s) => s.message)).toEqual(['Document deleted', 'Undone']);
		expect(shown[1]?.options.action).toBeUndefined();
	});

	it('leaves the view alone and passes the refusal on when the undo fails', async () => {
		const { deps, shown } = setup(async () => {
			throw new Error('a later change must be undone first');
		});
		const restored = vi.fn();
		announce(deps, 'Split into 2 documents', 'e2', restored);
		await expect(shown[0]?.options.action?.run()).rejects.toThrow(
			'a later change must be undone first',
		);
		expect(restored).not.toHaveBeenCalled();
		expect(shown).toHaveLength(1);
	});
});
