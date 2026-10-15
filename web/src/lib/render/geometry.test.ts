import { describe, expect, it } from 'vitest';
import { sourceUv, toClip } from './geometry';

describe('toClip', () => {
	it('maps the full program to the whole clip square, top-left first', () => {
		expect(toClip({ x: 0, y: 0, w: 1920, h: 1080 }, 1920, 1080)).toEqual([-1, 1, 2, 2]);
	});

	it('maps a centred quarter rectangle', () => {
		const [x, y, w, h] = toClip({ x: 480, y: 270, w: 960, h: 540 }, 1920, 1080);
		expect([x, y, w, h]).toEqual([-0.5, 0.5, 1, 1]);
	});
});

describe('sourceUv', () => {
	it('uses the crop in source pixels, clamped', () => {
		expect(sourceUv(1280, 720, 100, 100, { x: 320, y: 180, w: 640, h: 360 })).toEqual([
			0.25, 0.25, 0.75, 0.75
		]);
		expect(sourceUv(1280, 720, 100, 100, { x: -10, y: 0, w: 5000, h: 720 })).toEqual([0, 0, 1, 1]);
	});

	it('covers a wider destination by trimming the top and bottom of the source', () => {
		// A 4:3 camera in a 16:9 slot keeps the full width and shows the middle 3/4 height.
		const [u0, v0, u1, v1] = sourceUv(640, 480, 1920, 1080, null);
		expect(u0).toBe(0);
		expect(u1).toBe(1);
		expect(v0).toBeCloseTo(0.125);
		expect(v1).toBeCloseTo(0.875);
	});

	it('covers a taller destination by trimming the sides of the source', () => {
		const [u0, v0, u1, v1] = sourceUv(1920, 1080, 1080, 1080, null);
		expect(v0).toBe(0);
		expect(v1).toBe(1);
		expect(u0).toBeCloseTo(0.21875);
		expect(u1).toBeCloseTo(0.78125);
	});

	it('is the identity for matching aspects and degenerate sizes', () => {
		expect(sourceUv(1280, 720, 1920, 1080, null)).toEqual([0, 0, 1, 1]);
		expect(sourceUv(0, 0, 1920, 1080, null)).toEqual([0, 0, 1, 1]);
		expect(sourceUv(1280, 720, 0, 0, null)).toEqual([0, 0, 1, 1]);
	});
});
