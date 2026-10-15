import { describe, expect, it } from 'vitest';
import { SEQ_LEN, encodeFrame } from './ingest-ws';

describe('encodeFrame', () => {
	it('prefixes the tag with a big-endian u32 sequence number', () => {
		const tag = new Uint8Array([9, 0, 0, 1, 0, 0, 0, 0, 0, 0, 0, 0x17, 0, 0, 0, 12]);
		const message = encodeFrame(0xdeadbeef, tag);
		expect(message.byteLength).toBe(SEQ_LEN + tag.byteLength);
		expect(Array.from(message.subarray(0, 4))).toEqual([0xde, 0xad, 0xbe, 0xef]);
		expect(Array.from(message.subarray(4))).toEqual(Array.from(tag));
	});

	it('starts counting at one and keeps the prefix for small numbers', () => {
		expect(Array.from(encodeFrame(1, new Uint8Array([7])))).toEqual([0, 0, 0, 1, 7]);
	});
});
