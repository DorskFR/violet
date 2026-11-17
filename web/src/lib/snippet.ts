export type SnippetPart = { text: string; mark: boolean };

const ENTITIES: Record<string, string> = {
	amp: '&',
	lt: '<',
	gt: '>',
	quot: '"',
	apos: "'",
	nbsp: ' ',
};

function decodeEntities(text: string): string {
	return text.replace(/&(#x[0-9a-f]+|#\d+|[a-z]+);/gi, (whole, body: string) => {
		if (body[0] === '#') {
			const hex = body[1] === 'x' || body[1] === 'X';
			const code = Number.parseInt(body.slice(hex ? 2 : 1), hex ? 16 : 10);
			return Number.isInteger(code) && code > 0 && code <= 0x10ffff
				? String.fromCodePoint(code)
				: whole;
		}
		return ENTITIES[body.toLowerCase()] ?? whole;
	});
}

/**
 * Splits a server snippet into plain-text parts. Only `<mark>` and `</mark>` are understood as
 * markup; any other tag stays in the text, so rendering the parts as text nodes cannot inject HTML.
 */
export function parseSnippet(html: string | null | undefined): SnippetPart[] {
	if (!html) return [];
	const parts: SnippetPart[] = [];
	let depth = 0;
	for (const piece of html.split(/(<\/?mark>)/i)) {
		const tag = piece.toLowerCase();
		if (tag === '<mark>') {
			depth += 1;
		} else if (tag === '</mark>') {
			depth = Math.max(0, depth - 1);
		} else if (piece) {
			const text = decodeEntities(piece);
			const mark = depth > 0;
			const last = parts[parts.length - 1];
			if (last && last.mark === mark) last.text += text;
			else parts.push({ text, mark });
		}
	}
	return parts;
}
