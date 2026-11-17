import { describe, expect, it } from 'vitest';
import { describeGroup, splitGroups, toggleCut } from './split';

describe('splitGroups', () => {
	it('is one group without cuts', () => {
		expect(splitGroups(3, [])).toEqual([[1, 2, 3]]);
	});

	it('cuts after the given pages', () => {
		expect(splitGroups(3, [2])).toEqual([[1, 2], [3]]);
		expect(splitGroups(5, [1, 3])).toEqual([[1], [2, 3], [4, 5]]);
	});

	it('gives one page per group when every gap is cut', () => {
		expect(splitGroups(4, [1, 2, 3])).toEqual([[1], [2], [3], [4]]);
	});

	it('ignores cuts that are out of range, repeated or unordered', () => {
		expect(splitGroups(3, [0, 3, 7, -1, 1.5, 2, 2])).toEqual([[1, 2], [3]]);
		expect(splitGroups(4, new Set([3, 1]))).toEqual([[1], [2, 3], [4]]);
	});

	it('covers every page exactly once', () => {
		const groups = splitGroups(9, [2, 5, 8]);
		expect(groups.flat()).toEqual([1, 2, 3, 4, 5, 6, 7, 8, 9]);
	});

	it('handles one page and no pages', () => {
		expect(splitGroups(1, [1])).toEqual([[1]]);
		expect(splitGroups(0, [])).toEqual([]);
	});
});

describe('toggleCut', () => {
	it('adds in order and removes', () => {
		expect(toggleCut([3], 1)).toEqual([1, 3]);
		expect(toggleCut([1, 3], 3)).toEqual([1]);
	});
});

describe('describeGroup', () => {
	it('names a page or a range', () => {
		expect(describeGroup([2])).toBe('Page 2');
		expect(describeGroup([2, 3, 4])).toBe('Pages 2–4');
		expect(describeGroup([])).toBe('');
	});
});
