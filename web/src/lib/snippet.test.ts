import { describe, expect, it } from 'vitest';
import { parseSnippet } from './snippet';

describe('parseSnippet', () => {
	it('returns nothing for an empty snippet', () => {
		expect(parseSnippet(null)).toEqual([]);
		expect(parseSnippet('')).toEqual([]);
	});

	it('splits marked and plain text', () => {
		expect(parseSnippet('paid <mark>rent</mark> for May')).toEqual([
			{ text: 'paid ', mark: false },
			{ text: 'rent', mark: true },
			{ text: ' for May', mark: false },
		]);
	});

	it('decodes the entities the server escapes', () => {
		expect(parseSnippet('A &amp; B &lt;<mark>C&#39;s</mark>&gt; &quot;D&quot; &#x3042;')).toEqual([
			{ text: 'A & B <', mark: false },
			{ text: "C's", mark: true },
			{ text: '> "D" あ', mark: false },
		]);
	});

	it('keeps any other tag as text', () => {
		const parts = parseSnippet(
			'<script>alert(1)</script><img src=x onerror=alert(1)><mark>x</mark>',
		);
		expect(parts).toEqual([
			{ text: '<script>alert(1)</script><img src=x onerror=alert(1)>', mark: false },
			{ text: 'x', mark: true },
		]);
	});

	it('does not treat a mark tag with attributes as markup', () => {
		expect(parseSnippet('<mark onclick="x()">a</mark>')).toEqual([
			{ text: '<mark onclick="x()">a', mark: false },
		]);
	});

	it('does not double-decode', () => {
		expect(parseSnippet('&amp;lt;b&amp;gt;')).toEqual([{ text: '&lt;b&gt;', mark: false }]);
	});

	it('survives unbalanced and nested marks', () => {
		expect(parseSnippet('a</mark>b<mark>c<mark>d</mark>e</mark>f<mark>g')).toEqual([
			{ text: 'ab', mark: false },
			{ text: 'cde', mark: true },
			{ text: 'f', mark: false },
			{ text: 'g', mark: true },
		]);
	});

	it('handles Japanese text and upper-case tags', () => {
		expect(parseSnippet('<MARK>領収書</MARK> 東京ガス')).toEqual([
			{ text: '領収書', mark: true },
			{ text: ' 東京ガス', mark: false },
		]);
	});

	it('leaves unknown and invalid entities alone', () => {
		expect(parseSnippet('&bogus; &#0; &#x110000;')).toEqual([
			{ text: '&bogus; &#0; &#x110000;', mark: false },
		]);
	});
});
