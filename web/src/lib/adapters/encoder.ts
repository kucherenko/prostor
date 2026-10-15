// WebCodecs encoders, configured as ADR 0005 decides: H.264 Main 4.0 in AVCC format,
// realtime, constant bitrate, a keyframe every 60 frames, hardware first with a software
// fallback; AAC-LC 48 kHz stereo when the platform has it. Runs in the compositor Worker.
// Health rules from spike 0001: drop non-key frames before `encode()` when the queue
// exceeds 8, and count outputs, because hardware encoders drop silently.

import type { EncoderKind } from '#lib/render/messages';

export const VIDEO_CODEC = 'avc1.4d0028';
export const AUDIO_CODEC = 'mp4a.40.2';
export const KEYFRAME_INTERVAL = 60;
/** Frames waiting in the encoder above which non-key frames are skipped. */
export const MAX_ENCODE_QUEUE = 8;
/**
 * AAC-LC encoder priming: the samples of silence the encoder prepends, which delay the
 * content behind its timestamps. 2112 samples is the usual value (AudioToolbox, FDK);
 * the acceptance test measures it with a clap and this constant is tuned from that.
 */
export const AAC_PRIMING_SAMPLES = 2112;

export interface VideoChunk {
	timestampUs: number;
	key: boolean;
	data: Uint8Array;
	/** The AVCDecoderConfigurationRecord, with the first chunk and on reconfiguration. */
	description: AllowSharedBufferSource | null;
}

export interface AudioChunk {
	timestampUs: number;
	data: Uint8Array;
	/** The AudioSpecificConfig, with the first chunk. */
	description: AllowSharedBufferSource | null;
}

export interface VideoEncoderOptions {
	width: number;
	height: number;
	fps: number;
	bitrate: number;
	onChunk: (chunk: VideoChunk) => void;
	/** The encoder failed for good (after the software fallback, or in software). */
	onFatal: (message: string) => void;
	onKind: (kind: EncoderKind) => void;
}

export interface VideoEncoderStats {
	kind: EncoderKind;
	queue: number;
	submitted: number;
	output: number;
	/** Skipped before `encode()` plus not returned by the encoder. */
	dropped: number;
	keyframes: number;
	bitrate: number;
}

function videoConfig(
	opts: VideoEncoderOptions,
	hardware: 'prefer-hardware' | 'prefer-software',
	bitrate: number
): VideoEncoderConfig {
	return {
		codec: VIDEO_CODEC,
		width: opts.width,
		height: opts.height,
		framerate: opts.fps,
		bitrate,
		hardwareAcceleration: hardware,
		latencyMode: 'realtime',
		bitrateMode: 'constant',
		avc: { format: 'avc' }
	};
}

/** The H.264 encoder with the keyframe schedule, the queue policy and the fallback. */
export class VideoPipe {
	private encoder: VideoEncoder | null = null;
	private kind: EncoderKind = 'none';
	private bitrate: number;
	private submitted = 0;
	private output = 0;
	private skipped = 0;
	private keyframes = 0;
	private sinceKeyframe = KEYFRAME_INTERVAL;
	private keyframeRequested = false;
	private closed = false;

	private constructor(private readonly opts: VideoEncoderOptions) {
		this.bitrate = opts.bitrate;
	}

	/** Picks hardware when supported, else software; `null` when H.264 is unavailable. */
	static async create(opts: VideoEncoderOptions): Promise<VideoPipe | null> {
		const pipe = new VideoPipe(opts);
		for (const hardware of ['prefer-hardware', 'prefer-software'] as const) {
			if (await pipe.open(hardware)) return pipe;
		}
		return null;
	}

	private async open(hardware: 'prefer-hardware' | 'prefer-software'): Promise<boolean> {
		const config = videoConfig(this.opts, hardware, this.bitrate);
		let supported: boolean;
		try {
			supported = (await VideoEncoder.isConfigSupported(config)).supported === true;
		} catch {
			supported = false;
		}
		if (!supported) return false;
		const encoder = new VideoEncoder({
			output: (chunk, metadata) => this.onOutput(chunk, metadata),
			error: (error) => this.onError(hardware, error)
		});
		encoder.configure(config);
		this.encoder = encoder;
		this.kind = hardware === 'prefer-hardware' ? 'hardware' : 'software';
		this.sinceKeyframe = KEYFRAME_INTERVAL;
		this.opts.onKind(this.kind);
		return true;
	}

	private onOutput(chunk: EncodedVideoChunk, metadata?: EncodedVideoChunkMetadata) {
		this.output += 1;
		const data = new Uint8Array(chunk.byteLength);
		chunk.copyTo(data);
		this.opts.onChunk({
			timestampUs: chunk.timestamp,
			key: chunk.type === 'key',
			data,
			description: metadata?.decoderConfig?.description ?? null
		});
	}

	private onError(hardware: string, error: DOMException) {
		if (this.closed) return;
		const message = `${this.kind} encoder: ${error.message}`;
		this.encoder = null;
		if (hardware === 'prefer-hardware') {
			// Fall back once; the new encoder emits a fresh decoder configuration.
			void this.open('prefer-software').then((ok) => {
				if (!ok) this.opts.onFatal(message);
			});
		} else {
			this.opts.onFatal(message);
		}
	}

	/** Encodes (or skips) a frame and closes it. */
	encode(frame: VideoFrame): void {
		const encoder = this.encoder;
		if (!encoder || encoder.state !== 'configured') {
			frame.close();
			this.skipped += 1;
			return;
		}
		const keyFrame = this.keyframeRequested || this.sinceKeyframe >= KEYFRAME_INTERVAL;
		if (!keyFrame && encoder.encodeQueueSize > MAX_ENCODE_QUEUE) {
			frame.close();
			this.skipped += 1;
			this.sinceKeyframe += 1;
			return;
		}
		try {
			encoder.encode(frame, { keyFrame });
			this.submitted += 1;
			if (keyFrame) {
				this.keyframes += 1;
				this.keyframeRequested = false;
				this.sinceKeyframe = 1;
			} else {
				this.sinceKeyframe += 1;
			}
		} finally {
			frame.close();
		}
	}

	/** The next frame is a keyframe. */
	requestKeyframe(): void {
		this.keyframeRequested = true;
	}

	/** Changes the target bitrate (a reconfiguration; the encoder keeps running). */
	setBitrate(bitrate: number): void {
		if (bitrate === this.bitrate) return;
		this.bitrate = bitrate;
		const encoder = this.encoder;
		if (!encoder || encoder.state !== 'configured') return;
		const hardware = this.kind === 'hardware' ? 'prefer-hardware' : 'prefer-software';
		encoder.configure(videoConfig(this.opts, hardware, bitrate));
	}

	stats(): VideoEncoderStats {
		const queue = this.encoder?.encodeQueueSize ?? 0;
		return {
			kind: this.kind,
			queue,
			submitted: this.submitted,
			output: this.output,
			dropped: this.skipped + Math.max(0, this.submitted - this.output - queue),
			keyframes: this.keyframes,
			bitrate: this.bitrate
		};
	}

	/** Drains the encoder; chunks arrive through `onChunk` before this resolves. */
	async flush(): Promise<void> {
		const encoder = this.encoder;
		if (encoder && encoder.state === 'configured') {
			await encoder.flush().catch(() => undefined);
		}
	}

	close(): void {
		this.closed = true;
		if (this.encoder && this.encoder.state !== 'closed') this.encoder.close();
		this.encoder = null;
	}
}

export interface AudioEncoderOptions {
	sampleRate: number;
	channels: number;
	bitrate: number;
	onChunk: (chunk: AudioChunk) => void;
	onFatal: (message: string) => void;
}

/** The AAC-LC encoder; absent on platforms without one (Linux Chrome). */
export class AudioPipe {
	private encoder: AudioEncoder | null = null;
	private chunks = 0;
	private closed = false;

	private constructor(private readonly opts: AudioEncoderOptions) {}

	static async create(opts: AudioEncoderOptions): Promise<AudioPipe | null> {
		const config: AudioEncoderConfig = {
			codec: AUDIO_CODEC,
			sampleRate: opts.sampleRate,
			numberOfChannels: opts.channels,
			bitrate: opts.bitrate,
			// Raw AAC frames, no ADTS headers: what the FLV audio tag carries.
			aac: { format: 'aac' }
		} as AudioEncoderConfig;
		try {
			if (!(await AudioEncoder.isConfigSupported(config)).supported) return null;
		} catch {
			return null;
		}
		const pipe = new AudioPipe(opts);
		const encoder = new AudioEncoder({
			output: (chunk, metadata) => pipe.onOutput(chunk, metadata),
			error: (error) => {
				if (!pipe.closed) opts.onFatal(`audio encoder: ${error.message}`);
			}
		});
		encoder.configure(config);
		pipe.encoder = encoder;
		return pipe;
	}

	private onOutput(chunk: EncodedAudioChunk, metadata?: EncodedAudioChunkMetadata) {
		this.chunks += 1;
		const data = new Uint8Array(chunk.byteLength);
		chunk.copyTo(data);
		this.opts.onChunk({
			timestampUs: chunk.timestamp,
			data,
			description: metadata?.decoderConfig?.description ?? null
		});
	}

	/** Encodes and closes one `AudioData`. */
	encode(data: AudioData): void {
		const encoder = this.encoder;
		try {
			if (encoder && encoder.state === 'configured') encoder.encode(data);
		} finally {
			data.close();
		}
	}

	stats(): { chunks: number; queue: number } {
		return { chunks: this.chunks, queue: this.encoder?.encodeQueueSize ?? 0 };
	}

	async flush(): Promise<void> {
		const encoder = this.encoder;
		if (encoder && encoder.state === 'configured') {
			await encoder.flush().catch(() => undefined);
		}
	}

	close(): void {
		this.closed = true;
		if (this.encoder && this.encoder.state !== 'closed') this.encoder.close();
		this.encoder = null;
	}
}
