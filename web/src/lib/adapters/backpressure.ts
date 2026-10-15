// The ingest backpressure policy of ADR 0003 §4, as pure functions so it is unit-tested
// without a socket. The signal is the lag: the age of the oldest tag the server has not
// acknowledged (0 when nothing is outstanding), which spike 0004 showed bounds latency
// where `bufferedAmount` cannot.

export interface BackpressureConfig {
	/** Lag above which delta frames are dropped and the bitrate steps down. */
	dropLagMs: number;
	/** Lag above which the session is shown as degraded. */
	degradedLagMs: number;
	/** Lag above which the socket is reconnected. */
	reconnectLagMs: number;
	/** No ack for this long while tags are outstanding: reconnect. */
	ackTimeoutMs: number;
	/** Lag below which the bitrate may step back up. */
	raiseBelowLagMs: number;
	/** Multiplier per step down (0.75: minus 25 %). */
	stepDown: number;
	/** Multiplier per step up (slow). */
	stepUp: number;
	/** Shortest interval between two steps down. */
	stepDownHoldMs: number;
	/** Shortest interval between a step (either way) and a step up. */
	stepUpHoldMs: number;
	/** Shortest interval between two keyframe requests caused by lag. */
	keyframeHoldMs: number;
	/** The bitrate never steps below this. */
	minBitrate: number;
}

export const DEFAULT_BACKPRESSURE: BackpressureConfig = {
	dropLagMs: 500,
	degradedLagMs: 2000,
	reconnectLagMs: 10_000,
	ackTimeoutMs: 5000,
	raiseBelowLagMs: 100,
	stepDown: 0.75,
	stepUp: 1.1,
	stepDownHoldMs: 2000,
	stepUpHoldMs: 5000,
	keyframeHoldMs: 2000,
	minBitrate: 500_000
};

export interface BackpressureState {
	/** What the producer asked for; steps up never exceed it. */
	targetBitrate: number;
	/** The bitrate currently requested from the encoder. */
	bitrate: number;
	/** Delta frames are being dropped until the next keyframe. */
	dropping: boolean;
	lastStepDownAt: number;
	lastStepUpAt: number;
	lastKeyframeRequestAt: number;
}

export interface BackpressureInput {
	now: number;
	lagMs: number;
	/** Whether any tag is waiting for an ack (an ack timeout only counts then). */
	outstanding: boolean;
	/** Milliseconds since the last ack (or since the connection opened). */
	sinceAckMs: number;
}

export interface BackpressureActions {
	/** Ask the encoder for a keyframe now. */
	requestKeyframe: boolean;
	/** Reconfigure the encoder to this bitrate (bits per second). */
	setBitrate: number | null;
	degraded: boolean;
	reconnect: boolean;
}

export function initialState(targetBitrate: number, now = 0): BackpressureState {
	return {
		targetBitrate,
		bitrate: targetBitrate,
		dropping: false,
		lastStepDownAt: now,
		lastStepUpAt: now,
		lastKeyframeRequestAt: -Infinity
	};
}

/** Evaluates the policy for the current lag; mutates `state` and returns what to do. */
export function evaluate(
	state: BackpressureState,
	input: BackpressureInput,
	config: BackpressureConfig = DEFAULT_BACKPRESSURE
): BackpressureActions {
	const { now, lagMs } = input;
	const actions: BackpressureActions = {
		requestKeyframe: false,
		setBitrate: null,
		degraded: lagMs > config.degradedLagMs,
		reconnect:
			lagMs > config.reconnectLagMs || (input.outstanding && input.sinceAckMs > config.ackTimeoutMs)
	};
	if (lagMs > config.dropLagMs) {
		if (!state.dropping) {
			state.dropping = true;
			if (now - state.lastKeyframeRequestAt >= config.keyframeHoldMs) {
				state.lastKeyframeRequestAt = now;
				actions.requestKeyframe = true;
			}
		}
		if (now - state.lastStepDownAt >= config.stepDownHoldMs && state.bitrate > config.minBitrate) {
			state.bitrate = Math.max(config.minBitrate, roundKbit(state.bitrate * config.stepDown));
			state.lastStepDownAt = now;
			actions.setBitrate = state.bitrate;
		}
	} else if (
		lagMs < config.raiseBelowLagMs &&
		state.bitrate < state.targetBitrate &&
		now - state.lastStepUpAt >= config.stepUpHoldMs &&
		now - state.lastStepDownAt >= config.stepUpHoldMs
	) {
		state.bitrate = Math.min(state.targetBitrate, roundKbit(state.bitrate * config.stepUp));
		state.lastStepUpAt = now;
		actions.setBitrate = state.bitrate;
	}
	return actions;
}

/**
 * Whether a tag may be sent now. Delta frames are dropped while `dropping`; the next
 * keyframe ends the dropping (its deltas decode again). Audio, sequence headers and
 * keyframes are never dropped.
 */
export function admit(
	state: BackpressureState,
	tag: { kind: 'video' | 'audio'; key: boolean; sequenceHeader: boolean }
): boolean {
	if (tag.kind !== 'video' || tag.sequenceHeader) return true;
	if (tag.key) {
		state.dropping = false;
		return true;
	}
	return !state.dropping;
}

function roundKbit(bps: number): number {
	return Math.round(bps / 1000) * 1000;
}
