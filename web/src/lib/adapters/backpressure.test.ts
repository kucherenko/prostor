import { describe, expect, it } from 'vitest';
import { DEFAULT_BACKPRESSURE, admit, evaluate, initialState } from './backpressure';

const quiet = (now: number) => ({ now, lagMs: 20, outstanding: true, sinceAckMs: 50 });
const lagging = (now: number, lagMs = 800) => ({ now, lagMs, outstanding: true, sinceAckMs: 50 });

describe('backpressure policy', () => {
	it('does nothing while the lag is small', () => {
		const state = initialState(6_000_000);
		const actions = evaluate(state, quiet(100));
		expect(actions).toEqual({
			requestKeyframe: false,
			setBitrate: null,
			degraded: false,
			reconnect: false
		});
		expect(state.dropping).toBe(false);
		expect(state.bitrate).toBe(6_000_000);
	});

	it('drops deltas, asks for a keyframe and steps the bitrate down by 25 % on lag', () => {
		const state = initialState(6_000_000, 0);
		const actions = evaluate(state, lagging(3000));
		expect(actions.requestKeyframe).toBe(true);
		expect(actions.setBitrate).toBe(4_500_000);
		expect(actions.degraded).toBe(false);
		expect(state.dropping).toBe(true);

		// Non-key video is dropped until the next keyframe; audio always passes.
		expect(admit(state, { kind: 'video', key: false, sequenceHeader: false })).toBe(false);
		expect(admit(state, { kind: 'audio', key: false, sequenceHeader: false })).toBe(true);
		expect(admit(state, { kind: 'video', key: false, sequenceHeader: true })).toBe(true);
		expect(admit(state, { kind: 'video', key: true, sequenceHeader: false })).toBe(true);
		expect(state.dropping).toBe(false);
		expect(admit(state, { kind: 'video', key: false, sequenceHeader: false })).toBe(true);
	});

	it('holds further steps down and keyframe requests for the configured intervals', () => {
		const state = initialState(6_000_000, 0);
		evaluate(state, lagging(3000));
		admit(state, { kind: 'video', key: true, sequenceHeader: false });
		const again = evaluate(state, lagging(3500));
		expect(again.requestKeyframe).toBe(false);
		expect(again.setBitrate).toBeNull();
		expect(state.dropping).toBe(true);
		admit(state, { kind: 'video', key: true, sequenceHeader: false });
		const later = evaluate(state, lagging(5100));
		expect(later.requestKeyframe).toBe(true);
		expect(later.setBitrate).toBe(3_375_000);
	});

	it('never steps below the minimum bitrate', () => {
		const state = initialState(600_000, 0);
		evaluate(state, lagging(3000));
		expect(state.bitrate).toBe(DEFAULT_BACKPRESSURE.minBitrate);
		const more = evaluate(state, lagging(6000));
		expect(more.setBitrate).toBeNull();
	});

	it('steps back up slowly while the lag stays low, never above the target', () => {
		const state = initialState(6_000_000, 0);
		evaluate(state, lagging(3000));
		expect(state.bitrate).toBe(4_500_000);
		expect(evaluate(state, quiet(4000)).setBitrate).toBeNull();
		expect(evaluate(state, quiet(8001)).setBitrate).toBe(4_950_000);
		expect(evaluate(state, quiet(10_000)).setBitrate).toBeNull();
		expect(evaluate(state, quiet(13_002)).setBitrate).toBe(5_445_000);
		expect(evaluate(state, quiet(18_003)).setBitrate).toBe(5_990_000);
		expect(evaluate(state, quiet(23_004)).setBitrate).toBe(6_000_000);
		expect(evaluate(state, quiet(28_005)).setBitrate).toBeNull();
	});

	it('flags degraded and reconnect at the ADR thresholds', () => {
		const state = initialState(6_000_000, 0);
		expect(evaluate(state, lagging(1000, 2500)).degraded).toBe(true);
		expect(evaluate(state, lagging(1000, 2500)).reconnect).toBe(false);
		expect(evaluate(state, lagging(1000, 10_500)).reconnect).toBe(true);
		expect(
			evaluate(state, { now: 1000, lagMs: 0, outstanding: true, sinceAckMs: 5500 }).reconnect
		).toBe(true);
		expect(
			evaluate(state, { now: 1000, lagMs: 0, outstanding: false, sinceAckMs: 60_000 }).reconnect
		).toBe(false);
	});
});
