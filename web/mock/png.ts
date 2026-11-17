import { crc32, deflateSync } from 'node:zlib';

function chunk(type: string, data: Buffer): Buffer {
	const body = Buffer.concat([Buffer.from(type, 'latin1'), data]);
	const out = Buffer.alloc(body.length + 8);
	out.writeUInt32BE(data.length, 0);
	body.copy(out, 4);
	out.writeUInt32BE(crc32(body), body.length + 4);
	return out;
}

function encode(width: number, height: number, gray: Uint8Array): Buffer {
	const raw = Buffer.alloc((width + 1) * height);
	for (let y = 0; y < height; y += 1) {
		raw[y * (width + 1)] = 0;
		raw.set(gray.subarray(y * width, (y + 1) * width), y * (width + 1) + 1);
	}
	const header = Buffer.alloc(13);
	header.writeUInt32BE(width, 0);
	header.writeUInt32BE(height, 4);
	header[8] = 8;
	return Buffer.concat([
		Buffer.from([0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a]),
		chunk('IHDR', header),
		chunk('IDAT', deflateSync(raw, { level: 6 })),
		chunk('IEND', Buffer.alloc(0)),
	]);
}

function rng(seed: number): () => number {
	let s = seed >>> 0 || 1;
	return () => {
		s ^= s << 13;
		s ^= s >>> 17;
		s ^= s << 5;
		return ((s >>> 0) % 10000) / 10000;
	};
}

/** A fake scanned A4 page: a letterhead block, ruled "text" lines and a totals box. */
export function pagePng(seed: number, maxSide: number, rotation: number): Buffer {
	const h = Math.max(64, Math.min(1000, Math.round(maxSide)));
	const w = Math.round(h / Math.SQRT2);
	const page = new Uint8Array(w * h).fill(250);
	const rand = rng(seed);
	const rect = (x0: number, y0: number, x1: number, y1: number, shade: number) => {
		for (let y = Math.max(0, Math.round(y0 * h)); y < Math.min(h, Math.round(y1 * h)); y += 1) {
			for (let x = Math.max(0, Math.round(x0 * w)); x < Math.min(w, Math.round(x1 * w)); x += 1) {
				page[y * w + x] = shade;
			}
		}
	};
	rect(0.1, 0.06, 0.34 + rand() * 0.2, 0.085, 70);
	rect(0.1, 0.1, 0.25 + rand() * 0.15, 0.112, 170);
	rect(0.68, 0.06, 0.9, 0.072, 170);
	let y = 0.17;
	while (y < 0.78) {
		const lines = 2 + Math.floor(rand() * 5);
		for (let i = 0; i < lines && y < 0.78; i += 1) {
			const end = i === lines - 1 ? 0.3 + rand() * 0.5 : 0.84 + rand() * 0.06;
			rect(0.1, y, end, y + 0.009, 150 + Math.floor(rand() * 40));
			y += 0.022;
		}
		y += 0.02;
	}
	rect(0.55, 0.82, 0.9, 0.822, 110);
	rect(0.55, 0.84, 0.7, 0.852, 150);
	rect(0.76, 0.838, 0.9, 0.856, 60);
	rect(0.55, 0.875, 0.9, 0.877, 110);

	const turns = (((Math.round(rotation / 90) % 4) + 4) % 4) as 0 | 1 | 2 | 3;
	if (turns === 0) return encode(w, h, page);
	const ow = turns === 2 ? w : h;
	const oh = turns === 2 ? h : w;
	const out = new Uint8Array(ow * oh);
	for (let py = 0; py < h; py += 1) {
		for (let px = 0; px < w; px += 1) {
			const [ox, oy] =
				turns === 1 ? [h - 1 - py, px] : turns === 2 ? [w - 1 - px, h - 1 - py] : [py, w - 1 - px];
			out[oy * ow + ox] = page[py * w + px] ?? 250;
		}
	}
	return encode(ow, oh, out);
}
