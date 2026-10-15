// Capture on the main thread: the camera through `getUserMedia`, and the microphone
// through a Web Audio graph whose output track is read back as `AudioData`. Both are
// read with `MediaStreamTrackProcessor`, which Chrome offers in Window scope only, and
// handed to a sink that transfers them to the compositor Worker (ADR 0005 §2).

export interface CaptureConstraints {
	width: number;
	height: number;
	fps: number;
	audio: boolean;
}

export interface Pump {
	/** Stops reading; frames already handed to the sink stay the sink's to close. */
	stop(): Promise<void>;
}

export interface AudioGraph {
	context: AudioContext;
	/** The mixed output as a track, for a `MediaStreamTrackProcessor`. */
	track: MediaStreamTrack;
	/** Master gain of the (for now single-source) mix. */
	gain: GainNode;
	close(): Promise<void>;
}

/** Whether this browser has everything the producer pipeline needs. */
export function captureSupport(): { ok: boolean; missing: string[] } {
	const missing: string[] = [];
	const has = (name: string) =>
		typeof (globalThis as Record<string, unknown>)[name] !== 'undefined';
	if (!navigator.mediaDevices?.getUserMedia) missing.push('getUserMedia');
	if (!has('MediaStreamTrackProcessor')) missing.push('MediaStreamTrackProcessor');
	if (!has('VideoEncoder')) missing.push('VideoEncoder');
	if (!has('OffscreenCanvas')) missing.push('OffscreenCanvas');
	if (!has('Worker')) missing.push('Worker');
	return { ok: missing.length === 0, missing };
}

/** Opens the camera (and the microphone when `audio` is set). */
export async function openCapture(constraints: CaptureConstraints): Promise<MediaStream> {
	return navigator.mediaDevices.getUserMedia({
		video: {
			width: { ideal: constraints.width },
			height: { ideal: constraints.height },
			frameRate: { ideal: constraints.fps }
		},
		audio: constraints.audio
			? { echoCancellation: true, noiseSuppression: true, autoGainControl: true }
			: false
	});
}

/**
 * Reads every frame of a video track and hands it to `sink`, which takes ownership
 * (it must close or transfer the frame).
 */
export function pumpVideoFrames(track: MediaStreamTrack, sink: (frame: VideoFrame) => void): Pump {
	return pump(new MediaStreamTrackProcessor<VideoFrame>({ track }).readable, sink);
}

/** Reads every `AudioData` of an audio track and hands it to `sink` (which owns it). */
export function pumpAudioData(track: MediaStreamTrack, sink: (data: AudioData) => void): Pump {
	return pump(new MediaStreamTrackProcessor<AudioData>({ track }).readable, sink);
}

function pump<T extends VideoFrame | AudioData>(
	readable: ReadableStream<T>,
	sink: (item: T) => void
): Pump {
	const reader = readable.getReader();
	let stopped = false;
	const done = (async () => {
		for (;;) {
			const { value, done } = await reader.read();
			if (done || !value) break;
			if (stopped) {
				value.close();
				break;
			}
			sink(value);
		}
	})();
	return {
		async stop() {
			stopped = true;
			await reader.cancel().catch(() => undefined);
			await done.catch(() => undefined);
		}
	};
}

/**
 * Builds the audio graph: microphone → gain → a destination whose track carries the mix.
 * The sample rate is the encoder's, so no resampling happens between the two.
 */
export async function createAudioGraph(
	stream: MediaStream,
	sampleRate: number
): Promise<AudioGraph | null> {
	const [micTrack] = stream.getAudioTracks();
	if (!micTrack) return null;
	const context = new AudioContext({ sampleRate, latencyHint: 'interactive' });
	const source = context.createMediaStreamSource(new MediaStream([micTrack]));
	const gain = context.createGain();
	const destination = context.createMediaStreamDestination();
	source.connect(gain).connect(destination);
	await context.resume();
	const [track] = destination.stream.getAudioTracks();
	return {
		context,
		track,
		gain,
		async close() {
			source.disconnect();
			gain.disconnect();
			track.stop();
			await context.close();
		}
	};
}
