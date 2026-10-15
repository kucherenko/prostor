// Main-thread side of the producer pipeline: owns the Worker, the capture devices and
// the preview canvas, and forwards camera frames and mixed audio to the Worker.

import type { RenderPlan } from '#lib/generated/RenderPlan';
import {
	createAudioGraph,
	openCapture,
	pumpAudioData,
	pumpVideoFrames,
	type AudioGraph,
	type Pump
} from '#lib/adapters/media';
import type { FromWorker, PipelineStatus, SessionConfig, ToWorker } from './messages';

export interface PipelineEvents {
	onStatus: (status: PipelineStatus) => void;
	onStarted: (recording: string | null, audio: boolean) => void;
	onStopped: (status: PipelineStatus, reason: string | null) => void;
	onError: (message: string) => void;
}

export class StudioPipeline {
	private worker: Worker | null = null;
	private stream: MediaStream | null = null;
	private videoPump: Pump | null = null;
	private audioPump: Pump | null = null;
	private audioGraph: AudioGraph | null = null;
	private waiting: { resolve: (message: FromWorker) => void; types: FromWorker['type'][] }[] = [];

	constructor(private readonly events: PipelineEvents) {}

	/** Starts the Worker and hands it the preview canvas and the plan. */
	async init(previewCanvas: HTMLCanvasElement | null, plan: RenderPlan): Promise<void> {
		if (this.worker) return;
		const worker = new Worker(new URL('./compositor.worker.ts', import.meta.url), {
			type: 'module'
		});
		worker.onmessage = (event: MessageEvent<FromWorker>) => this.onMessage(event.data);
		worker.onerror = (event) => this.events.onError(`worker: ${event.message}`);
		this.worker = worker;
		const preview = previewCanvas?.transferControlToOffscreen() ?? null;
		const ready = this.waitFor(['ready', 'error']);
		this.send({ type: 'init', preview, plan }, preview ? [preview] : []);
		const message = await ready;
		if (message.type === 'error') throw new Error(message.message);
	}

	setPlan(plan: RenderPlan): void {
		this.send({ type: 'plan', plan });
	}

	/** Opens the camera (and microphone) and starts forwarding frames to the Worker. */
	async openMedia(opts: {
		width: number;
		height: number;
		fps: number;
		audio: boolean;
		sampleRate: number;
	}): Promise<{ video: MediaTrackSettings; audio: boolean }> {
		await this.closeMedia();
		const stream = await openCapture(opts);
		this.stream = stream;
		const [videoTrack] = stream.getVideoTracks();
		if (!videoTrack) throw new Error('no camera track');
		this.videoPump = pumpVideoFrames(videoTrack, (frame) => {
			this.send({ type: 'camera', frame }, [frame]);
		});
		let audio = false;
		if (opts.audio) {
			this.audioGraph = await createAudioGraph(stream, opts.sampleRate);
			if (this.audioGraph) {
				audio = true;
				this.audioPump = pumpAudioData(this.audioGraph.track, (data) => {
					this.send({ type: 'audio', data }, [data]);
				});
			}
		}
		return { video: videoTrack.getSettings(), audio };
	}

	async closeMedia(): Promise<void> {
		await this.videoPump?.stop();
		await this.audioPump?.stop();
		this.videoPump = null;
		this.audioPump = null;
		await this.audioGraph?.close();
		this.audioGraph = null;
		this.stream?.getTracks().forEach((track) => track.stop());
		this.stream = null;
	}

	/** Starts a session; resolves with the recording name once the server answered. */
	async start(config: SessionConfig): Promise<{ recording: string | null; audio: boolean }> {
		const started = this.waitFor(['started', 'error', 'stopped']);
		this.send({ type: 'start', config });
		const message = await started;
		if (message.type === 'started') return { recording: message.recording, audio: message.audio };
		if (message.type === 'error') throw new Error(message.message);
		if (message.type === 'stopped') {
			throw new Error(message.reason ?? 'session stopped before it started');
		}
		throw new Error(`unexpected ${message.type}`);
	}

	/** Ends the session: flushes the encoders, sends `end_of_stream`, waits for `ended`. */
	async stop(): Promise<PipelineStatus> {
		const stopped = this.waitFor(['stopped']);
		this.send({ type: 'stop' });
		const message = await stopped;
		return message.type === 'stopped'
			? message.status
			: (() => {
					throw new Error('unexpected message');
				})();
	}

	async dispose(): Promise<void> {
		await this.closeMedia();
		this.worker?.terminate();
		this.worker = null;
	}

	private send(message: ToWorker, transfer: Transferable[] = []) {
		this.worker?.postMessage(message, transfer);
	}

	private waitFor(types: FromWorker['type'][]): Promise<FromWorker> {
		return new Promise((resolve) => this.waiting.push({ resolve, types }));
	}

	private onMessage(message: FromWorker) {
		const index = this.waiting.findIndex((w) => w.types.includes(message.type));
		if (index >= 0) {
			const [waiter] = this.waiting.splice(index, 1);
			waiter.resolve(message);
		}
		switch (message.type) {
			case 'status':
				this.events.onStatus(message.status);
				break;
			case 'started':
				this.events.onStarted(message.recording, message.audio);
				break;
			case 'stopped':
				this.events.onStopped(message.status, message.reason);
				break;
			case 'error':
				this.events.onError(message.message);
				break;
			case 'ready':
				break;
		}
	}
}
