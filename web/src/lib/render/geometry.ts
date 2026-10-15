// Pure rectangle math for the compositor: program pixels to clip space, and the source
// region a layer samples (its crop, or a centred "cover" fit of the source's aspect).

import type { Rect } from '#lib/generated/Rect';

/** A layer's destination in clip space: top-left corner, width, and height (downwards). */
export type ClipRect = [x: number, y: number, w: number, h: number];

/** Texture coordinates of the sampled region: `[u0, v0, u1, v1]`, v = 0 at the top. */
export type UvRect = [u0: number, v0: number, u1: number, v1: number];

/** Maps a program-pixel rectangle (origin top-left, y down) to clip space. */
export function toClip(rect: Rect, width: number, height: number): ClipRect {
	return [
		(rect.x / width) * 2 - 1,
		1 - (rect.y / height) * 2,
		(rect.w / width) * 2,
		(rect.h / height) * 2
	];
}

/**
 * The source region to draw into a `dstW × dstH` rectangle. With a crop (source pixels)
 * that region is the crop, clamped to the source. Without, the source is scaled to cover
 * the destination and centred, so a 4:3 camera fills a 16:9 slot without distortion.
 */
export function sourceUv(
	srcW: number,
	srcH: number,
	dstW: number,
	dstH: number,
	crop: Rect | null
): UvRect {
	if (srcW <= 0 || srcH <= 0) return [0, 0, 1, 1];
	if (crop) {
		const clamp = (v: number) => Math.min(1, Math.max(0, v));
		return [
			clamp(crop.x / srcW),
			clamp(crop.y / srcH),
			clamp((crop.x + crop.w) / srcW),
			clamp((crop.y + crop.h) / srcH)
		];
	}
	if (dstW <= 0 || dstH <= 0) return [0, 0, 1, 1];
	const srcAspect = srcW / srcH;
	const dstAspect = dstW / dstH;
	if (srcAspect > dstAspect) {
		const visible = dstAspect / srcAspect;
		const u0 = (1 - visible) / 2;
		return [u0, 0, 1 - u0, 1];
	}
	const visible = srcAspect / dstAspect;
	const v0 = (1 - visible) / 2;
	return [0, v0, 1, 1 - v0];
}
