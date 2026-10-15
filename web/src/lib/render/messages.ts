// Messages between the studio page and the compositor Worker, and the status the Worker
// reports. The page owns the camera and the microphone (Window-only APIs); the Worker
// owns rendering, encoding, muxing and the ingest socket (ADR 0005 §2, ADR 0003 §1).

import type { EgressState } from '#lib/generated/EgressState';
import type { RenderPlan } from '#lib/generated/RenderPlan';

/** What a session streams and where. The secret lives here in memory only. */
export interface SessionConfig {
	/** `ws(s)://host/ws/ingest/<room>` */
	url: string;
	/** The producer secret for `hello.auth`, or `null` when the server has none. */
	auth: string | null;
	width: number;
	height: number;
	fps: number;
	/** Target video bitrate in bits per second; the policy may step it down. */
	videoBitrate: number;
	/** Encode audio (AAC-LC). Falls back to video only when the browser has no AAC. */
	audio: boolean;
	audioBitrate: number;
	sampleRate: number;
	channels: number;
}

export type ToWorker =
	| { type: 'init'; preview: OffscreenCanvas | null; plan: RenderPlan }
	| { type: 'plan'; plan: RenderPlan }
	| { type: 'start'; config: SessionConfig }
	| { type: 'camera'; frame: VideoFrame }
	| { type: 'audio'; data: AudioData }
	| { type: 'stop' };

export type FromWorker =
	| { type: 'ready' }
	| { type: 'started'; recording: string | null; audio: boolean }
	| { type: 'status'; status: PipelineStatus }
	| { type: 'stopped'; status: PipelineStatus; reason: string | null }
	| { type: 'error'; message: string };

export type IngestPhase =
	'idle' | 'connecting' | 'live' | 'degraded' | 'reconnecting' | 'ending' | 'closed';

export type EncoderKind = 'none' | 'hardware' | 'software';

export interface EncoderStatus {
	kind: EncoderKind;
	/** `VideoEncoder.encodeQueueSize`; healthy is ≤ 1. */
	queue: number;
	submitted: number;
	output: number;
	/** Frames not encoded: skipped before `encode()` plus silently dropped by the encoder. */
	dropped: number;
	keyframes: number;
	/** Bits per second currently requested. */
	bitrate: number;
	targetBitrate: number;
}

export interface AudioStatus {
	enabled: boolean;
	chunks: number;
	queue: number;
	/** Milliseconds subtracted from audio timestamps for the AAC encoder's priming. */
	primingMs: number;
}

export interface IngestStatus {
	phase: IngestPhase;
	/** Age of the oldest unacknowledged tag; 0 when nothing is outstanding. */
	lagMs: number;
	bufferedAmount: number;
	sentTags: number;
	sentBytes: number;
	droppedTags: number;
	ackedSeq: number;
	lastSeq: number;
	reconnects: number;
	/** Outgoing bitrate over the last second, in kbit/s. */
	kbps: number;
	recording: string | null;
	egress: EgressState[];
}

export interface PipelineStatus {
	live: boolean;
	/** Frames rendered in the last second. */
	fps: number;
	framesRendered: number;
	cameraFrames: number;
	encoder: EncoderStatus;
	audio: AudioStatus;
	ingest: IngestStatus;
}

export function emptyStatus(): PipelineStatus {
	return {
		live: false,
		fps: 0,
		framesRendered: 0,
		cameraFrames: 0,
		encoder: {
			kind: 'none',
			queue: 0,
			submitted: 0,
			output: 0,
			dropped: 0,
			keyframes: 0,
			bitrate: 0,
			targetBitrate: 0
		},
		audio: { enabled: false, chunks: 0, queue: 0, primingMs: 0 },
		ingest: {
			phase: 'idle',
			lagMs: 0,
			bufferedAmount: 0,
			sentTags: 0,
			sentBytes: 0,
			droppedTags: 0,
			ackedSeq: 0,
			lastSeq: 0,
			reconnects: 0,
			kbps: 0,
			recording: null,
			egress: []
		}
	};
}
