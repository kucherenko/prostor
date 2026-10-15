// Capture on the main thread: the camera through `getUserMedia` (opened by the lobby), and
// the microphone through a Web Audio graph whose output track is read back as `AudioData`.
// Both are read with `MediaStreamTrackProcessor`, which Chrome offers in Window scope only,
// and handed to a sink that transfers them to the compositor Worker (ADR 0005 §2).

export interface CaptureConstraints {
	width: number;
	height: number;
	fps: number;
	audio: boolean;
	/** A camera that is plugged in (see `listDevices`); left out, the browser picks. */
	cameraId?: string | null;
	/** A microphone that is plugged in; left out, the browser picks. */
	microphoneId?: string | null;
}

/** What the lobby asks of a camera; the compositor scales whatever arrives into the program. */
export const CAMERA_CONSTRAINTS = { width: 1280, height: 720, fps: 30 } as const;

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

/**
 * The `getUserMedia` constraints for `constraints`. A device id becomes `exact`, which
 * fails for an id that is not plugged in: pass only ids taken from `listDevices`.
 */
export function captureConstraints(constraints: CaptureConstraints): MediaStreamConstraints {
	return {
		video: {
			width: { ideal: constraints.width },
			height: { ideal: constraints.height },
			frameRate: { ideal: constraints.fps },
			...(constraints.cameraId ? { deviceId: { exact: constraints.cameraId } } : {})
		},
		audio: constraints.audio
			? {
					echoCancellation: true,
					noiseSuppression: true,
					autoGainControl: true,
					...(constraints.microphoneId ? { deviceId: { exact: constraints.microphoneId } } : {})
				}
			: false
	};
}

/**
 * Opens the camera (and the microphone when `audio` is set). A device that vanished since
 * it was listed (`OverconstrainedError`) falls back to the browser's defaults, and a
 * machine without a microphone gets a stream without audio: the camera is the essential
 * device. Permission and missing-camera errors are thrown for the caller to show.
 */
export async function openCapture(constraints: CaptureConstraints): Promise<MediaStream> {
	try {
		return await getStream(constraints);
	} catch (error) {
		if (
			errorName(error) === 'OverconstrainedError' &&
			(constraints.cameraId || constraints.microphoneId)
		) {
			return getStream({ ...constraints, cameraId: null, microphoneId: null });
		}
		throw error;
	}
}

async function getStream(constraints: CaptureConstraints): Promise<MediaStream> {
	try {
		return await navigator.mediaDevices.getUserMedia(captureConstraints(constraints));
	} catch (error) {
		if (constraints.audio && errorName(error) === 'NotFoundError') {
			// Either device may be the missing one; without a camera this fails again, with
			// the same error, and that one is the caller's to show.
			return navigator.mediaDevices.getUserMedia(
				captureConstraints({ ...constraints, audio: false })
			);
		}
		throw error;
	}
}

function errorName(error: unknown): string {
	const name = (error as { name?: unknown } | null)?.name;
	return typeof name === 'string' ? name : '';
}

/** A sentence for the lobby about why the camera could not be opened. */
export function describeCaptureError(error: unknown): string {
	switch (errorName(error)) {
		case 'NotAllowedError':
			return 'Camera access was blocked. Allow the camera and microphone for this site in the browser, then try again.';
		case 'NotFoundError':
			return 'No camera found. Connect one, then try again.';
		case 'NotReadableError':
		case 'AbortError':
			return 'The camera could not be started. Another app may be using it: close that app, then try again.';
		default:
			return `Could not open the camera: ${error instanceof Error ? error.message : String(error)}`;
	}
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
