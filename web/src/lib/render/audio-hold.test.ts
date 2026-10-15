import { describe, expect, it } from 'vitest';
import { holdAudio } from './audio-hold';

interface Chunk {
	timestampUs: number;
	description: string | null;
}

function chunks(count: number, configuredFirst = true): Chunk[] {
	return Array.from({ length: count }, (_, i) => ({
		timestampUs: i * 21_333,
		description: configuredFirst && i === 0 ? 'asc' : null
	}));
}

function holdAll(input: Chunk[], max: number): Chunk[] {
	const held: Chunk[] = [];
	for (const chunk of input) holdAudio(held, chunk, max);
	return held;
}

describe('holdAudio', () => {
	it('keeps every chunk, in order, up to the limit', () => {
		const input = chunks(3);
		expect(holdAll(input, 3)).toEqual(input);
	});

	it('evicts the oldest chunks beyond the limit', () => {
		const held = holdAll(chunks(5), 3);
		expect(held.map((c) => c.timestampUs)).toEqual([2, 3, 4].map((i) => i * 21_333));
	});

	it('moves the decoder configuration of an evicted chunk to the new oldest one', () => {
		const held = holdAll(chunks(4), 3);
		expect(held[0]).toEqual({ timestampUs: 21_333, description: 'asc' });
		expect(held.slice(1).every((c) => c.description === null)).toBe(true);
	});

	it('keeps the configuration through many evictions (a slow video encoder)', () => {
		const held = holdAll(chunks(500), 50);
		expect(held).toHaveLength(50);
		expect(held[0]).toEqual({ timestampUs: 450 * 21_333, description: 'asc' });
	});

	it('does not overwrite a configuration the next chunk carries itself', () => {
		const input = chunks(3);
		input[1] = { ...input[1], description: 'newer' };
		const held = holdAll(input, 2);
		expect(held[0].description).toBe('newer');
	});

	it('does not invent a configuration when none was evicted', () => {
		const held = holdAll(chunks(4, false), 2);
		expect(held.every((c) => c.description === null)).toBe(true);
	});

	it('does not mutate the chunk objects it was given', () => {
		const input = chunks(2);
		holdAll(input, 1);
		expect(input[1].description).toBeNull();
	});
});
