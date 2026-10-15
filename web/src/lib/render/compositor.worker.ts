// The compositor Worker: renders the plan at the program's frame rate, encodes every
// frame with WebCodecs, muxes the chunks into FLV tags with the wasm muxer and sends
// them over the ingest socket. Camera frames and mixed audio arrive from the page.
// Everything here runs off the main thread so UI work cannot delay frames and
// background tabs cannot throttle the sender (ADR 0003 §1, ADR 0005 §2).

import type { StreamInfo } from '#lib/generated/StreamInfo';
import { AAC_PRIMING_SAMPLES, AudioPipe, VideoPipe } from '#lib/adapters/encoder';
import { IngestClient } from '#lib/adapters/ingest-ws';
import { FlvMuxer } from '#lib/wasm/pkg/prostor_wasm.js';
import { loadWasm } from '#lib/wasm';
import { Renderer } from './renderer';
import {
	emptyStatus,
	type FromWorker,
	type PipelineStatus,
	type SessionConfig,
	type ToWorker
} from './messages';

interface WorkerScope {
	postMessage(message: FromWorker, transfer?: Transferable[]): void;
	onmessage: ((event: MessageEvent<ToWorker>) => void) | null;
}

const scope = self as unknown as WorkerScope;
const STATUS_INTERVAL_MS = 500;
/** About one second of AAC chunks (47 per second at 48 kHz). */
const MAX_HELD_AUDIO = 50;

interface AudioChunkLike {
	timestampUs: number;
	data: Uint8Array;
	description: AllowSharedBufferSource | null;
}

let renderer: Renderer | null = null;
let preview: ImageBitmapRenderingContext | null = null;
let session: Session | null = null;
let frameTimer: ReturnType<typeof setTimeout> | null = null;
let framesRendered = 0;
let cameraFrames = 0;
let fpsWindow: number[] = [];
let statusTimer: ReturnType<typeof setInterval> | null = null;

function post(message: FromWorker, transfer: Transferable[] = []) {
	scope.postMessage(message, transfer);
}

function fail(message: string) {
	post({ type: 'error', message });
}

/** One live session: encoders, muxer and socket. */
class Session {
	readonly config: SessionConfig;
	readonly muxer: FlvMuxer;
	readonly originMs: number;
	video: VideoPipe | null = null;
	audio: AudioPipe | null = null;
	ingest: IngestClient | null = null;
	audioOffsetUs: number | null = null;
	/** The server wants the AVC sequence header first; audio that arrives earlier waits. */
	private videoHeaderSent = false;
	private heldAudio: AudioChunkLike[] = [];
	readonly primingMs: number;
	stopping = false;
	fatal: string | null = null;

	constructor(config: SessionConfig) {
		this.config = config;
		this.muxer = new FlvMuxer(true, config.audio);
		// One origin for both tracks: now, before any frame is encoded.
		this.originMs = performance.now();
		this.muxer.set_origin(this.originMs);
		this.primingMs = (AAC_PRIMING_SAMPLES / config.sampleRate) * 1000;
	}

	async start(): Promise<{ recording: string | null; audio: boolean }> {
		const { config } = this;
		this.video = await VideoPipe.create({
			width: config.width,
			height: config.height,
			fps: config.fps,
			bitrate: config.videoBitrate,
			onChunk: (chunk) => this.onVideoChunk(chunk),
			onFatal: (message) => this.abort(message),
			onKind: () => undefined
		});
		if (!this.video) throw new Error('no H.264 encoder available (WebCodecs)');
		if (config.audio) {
			this.audio = await AudioPipe.create({
				sampleRate: config.sampleRate,
				channels: config.channels,
				bitrate: config.audioBitrate,
				onChunk: (chunk) => this.onAudioChunk(chunk),
				onFatal: (message) => this.abort(message)
			});
		}
		const hasAudio = this.audio !== null;
		if (config.audio && !hasAudio) {
			// No AAC on this platform (ADR 0005 §6): stream video only.
			this.muxer.free();
			(this as { muxer: FlvMuxer }).muxer = new FlvMuxer(true, false);
			this.muxer.set_origin(this.originMs);
		}
		const stream: StreamInfo = {
			width: config.width,
			height: config.height,
			fps: config.fps,
			video_codec: 'avc1.4d0028',
			video_bitrate: config.videoBitrate,
			audio_codec: hasAudio ? 'mp4a.40.2' : null,
			audio_bitrate: hasAudio ? config.audioBitrate : null,
			sample_rate: hasAudio ? config.sampleRate : null,
			channels: hasAudio ? config.channels : null
		};
		this.ingest = new IngestClient({
			url: config.url,
			auth: config.auth,
			stream,
			onKeyframeRequest: () => this.video?.requestKeyframe(),
			onBitrate: (bitrate) => this.video?.setBitrate(bitrate),
			onFatal: (message) => this.abort(message)
		});
		const { recording } = await this.ingest.connect();
		return { recording, audio: hasAudio };
	}

	private onVideoChunk(chunk: {
		timestampUs: number;
		key: boolean;
		data: Uint8Array;
		description: AllowSharedBufferSource | null;
	}) {
		const ingest = this.ingest;
		if (!ingest || this.stopping) return;
		const timestampMs = chunk.timestampUs / 1000;
		try {
			if (chunk.description) {
				const header = this.muxer.video_sequence_header(
					timestampMs,
					chunk.description as BufferSource
				);
				ingest.send({ bytes: header, kind: 'video', key: true, sequenceHeader: true });
				if (!this.videoHeaderSent) {
					this.videoHeaderSent = true;
					for (const held of this.heldAudio.splice(0)) this.sendAudio(held);
				}
			} else if (!this.videoHeaderSent) {
				this.abort('video encoder delivered a frame before its decoder configuration');
				return;
			}
			const tag = this.muxer.video(timestampMs, chunk.key, 0, chunk.data);
			ingest.send({ bytes: tag, kind: 'video', key: chunk.key, sequenceHeader: false });
		} catch (error) {
			this.abort(`muxing video: ${describe(error)}`);
		}
	}

	private onAudioChunk(chunk: AudioChunkLike) {
		if (!this.ingest || this.stopping || this.audioOffsetUs === null) return;
		if (!this.videoHeaderSent) {
			// The AAC encoder usually answers first; the protocol wants the AVC header
			// before any media, so hold a little audio back (never more than a second).
			this.heldAudio.push(chunk);
			if (this.heldAudio.length > MAX_HELD_AUDIO) this.heldAudio.shift();
			return;
		}
		this.sendAudio(chunk);
	}

	private sendAudio(chunk: AudioChunkLike) {
		const ingest = this.ingest;
		if (!ingest || this.audioOffsetUs === null) return;
		// The encoder echoes the capture clock; map it onto the session clock and pull
		// the content forward by the encoder's priming delay.
		const timestampMs = (chunk.timestampUs + this.audioOffsetUs) / 1000 - this.primingMs;
		try {
			if (chunk.description) {
				const header = this.muxer.audio_sequence_header(
					timestampMs,
					chunk.description as BufferSource
				);
				ingest.send({ bytes: header, kind: 'audio', key: false, sequenceHeader: true });
			}
			const tag = this.muxer.audio(timestampMs, chunk.data);
			ingest.send({ bytes: tag, kind: 'audio', key: false, sequenceHeader: false });
		} catch (error) {
			this.abort(`muxing audio: ${describe(error)}`);
		}
	}

	/** Feeds mixed audio; the first chunk pins the capture clock to the session clock. */
	pushAudio(data: AudioData) {
		if (!this.audio || this.stopping) {
			data.close();
			return;
		}
		this.audioOffsetUs ??= performance.now() * 1000 - data.timestamp;
		this.audio.encode(data);
	}

	abort(message: string) {
		if (this.fatal) return;
		this.fatal = message;
		fail(message);
		void stopSession(message);
	}

	async stop(): Promise<void> {
		this.stopping = false;
		await this.video?.flush();
		await this.audio?.flush();
		this.stopping = true;
		this.video?.close();
		this.audio?.close();
		await this.ingest?.end();
		this.muxer.free();
	}

	status(): PipelineStatus {
		const status = emptyStatus();
		status.live = !this.stopping && this.fatal === null;
		const video = this.video?.stats();
		if (video) {
			status.encoder = { ...video, targetBitrate: this.config.videoBitrate };
		}
		const audio = this.audio?.stats();
		status.audio = {
			enabled: audio !== undefined,
			chunks: audio?.chunks ?? 0,
			queue: audio?.queue ?? 0,
			primingMs: audio ? Math.round(this.primingMs * 10) / 10 : 0
		};
		if (this.ingest) status.ingest = this.ingest.status();
		return status;
	}
}

function describe(error: unknown): string {
	return error instanceof Error ? error.message : String(error);
}

function currentStatus(): PipelineStatus {
	const status = session?.status() ?? emptyStatus();
	status.framesRendered = framesRendered;
	status.cameraFrames = cameraFrames;
	const now = performance.now();
	fpsWindow = fpsWindow.filter((at) => now - at <= 1000);
	status.fps = fpsWindow.length;
	return status;
}

function publishStatus() {
	session?.ingest?.tick();
	post({ type: 'status', status: currentStatus() });
}

/** The frame loop: a drift-corrected timer, since Workers get no display vsync. */
function startFrameLoop(fps: number) {
	stopFrameLoop();
	const periodMs = 1000 / fps;
	const base = performance.now();
	let n = 0;
	const tick = () => {
		n += 1;
		renderFrame(Math.round(periodMs * 1000));
		const next = base + n * periodMs;
		frameTimer = setTimeout(tick, Math.max(0, next - performance.now()));
	};
	frameTimer = setTimeout(tick, periodMs);
}

function stopFrameLoop() {
	if (frameTimer !== null) clearTimeout(frameTimer);
	frameTimer = null;
}

function renderFrame(durationUs: number) {
	if (!renderer) return;
	renderer.draw();
	framesRendered += 1;
	const now = performance.now();
	fpsWindow.push(now);
	if (session && !session.stopping && session.video) {
		const frame = new VideoFrame(renderer.canvas, {
			timestamp: Math.round(now * 1000),
			duration: durationUs
		});
		session.video.encode(frame);
	}
	if (preview) {
		preview.transferFromImageBitmap(renderer.canvas.transferToImageBitmap());
	}
}

async function startSession(config: SessionConfig) {
	if (session) {
		fail('a session is already running');
		return;
	}
	if (!renderer) {
		fail('the compositor is not initialised');
		return;
	}
	try {
		await loadWasm();
		const next = new Session(config);
		session = next;
		const { recording, audio } = await next.start();
		if (next.fatal) return;
		post({ type: 'started', recording, audio });
	} catch (error) {
		const message = describe(error);
		session = null;
		fail(message);
	}
}

async function stopSession(reason: string | null) {
	const current = session;
	if (!current) return;
	session = null;
	try {
		await current.stop();
	} finally {
		const status = current.status();
		status.framesRendered = framesRendered;
		status.cameraFrames = cameraFrames;
		post({ type: 'stopped', status, reason });
	}
}

scope.onmessage = (event: MessageEvent<ToWorker>) => {
	const message = event.data;
	switch (message.type) {
		case 'init': {
			try {
				renderer = new Renderer(message.plan);
				preview = message.preview?.getContext('bitmaprenderer') ?? null;
				statusTimer ??= setInterval(publishStatus, STATUS_INTERVAL_MS);
				startFrameLoop(30);
				post({ type: 'ready' });
			} catch (error) {
				fail(`compositor: ${describe(error)}`);
			}
			break;
		}
		case 'plan':
			renderer?.setPlan(message.plan);
			break;
		case 'camera':
			cameraFrames += 1;
			if (renderer) renderer.setCameraFrame(message.frame);
			else message.frame.close();
			break;
		case 'audio':
			if (session) session.pushAudio(message.data);
			else message.data.close();
			break;
		case 'start':
			startFrameLoop(message.config.fps);
			void startSession(message.config);
			break;
		case 'stop':
			void stopSession(null);
			break;
	}
};
