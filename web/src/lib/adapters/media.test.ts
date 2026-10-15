import { afterEach, describe, expect, it, vi } from 'vitest';
import {
	CAMERA_CONSTRAINTS,
	captureConstraints,
	describeCaptureError,
	openCapture,
	type CaptureConstraints
} from './media';

const base: CaptureConstraints = { ...CAMERA_CONSTRAINTS, audio: true };

describe('captureConstraints', () => {
	it('asks for 1280x720 @ 30 and leaves the devices to the browser by default', () => {
		expect(captureConstraints(base)).toEqual({
			video: { width: { ideal: 1280 }, height: { ideal: 720 }, frameRate: { ideal: 30 } },
			audio: { echoCancellation: true, noiseSuppression: true, autoGainControl: true }
		});
		expect(captureConstraints({ ...base, cameraId: null, microphoneId: '' })).toEqual(
			captureConstraints(base)
		);
	});

	it('makes a given device id exact', () => {
		const constraints = captureConstraints({ ...base, cameraId: 'cam-1', microphoneId: 'mic-1' });
		expect(constraints.video).toMatchObject({ deviceId: { exact: 'cam-1' } });
		expect(constraints.audio).toMatchObject({ deviceId: { exact: 'mic-1' } });
	});

	it('requests no audio, whatever the microphone id', () => {
		expect(captureConstraints({ ...base, audio: false, microphoneId: 'mic-1' }).audio).toBe(false);
	});
});

describe('openCapture', () => {
	afterEach(() => vi.unstubAllGlobals());

	/** A `DOMException`-like failure: the lobby only looks at `name`. */
	const failure = (name: string) => Object.assign(new Error(name), { name });

	/** Stubs `navigator.mediaDevices.getUserMedia` with one outcome per call. */
	function stubGetUserMedia(...outcomes: (MediaStream | Error)[]) {
		const getUserMedia = vi.fn<(constraints: MediaStreamConstraints) => Promise<MediaStream>>(
			async () => {
				const next = outcomes.shift();
				if (next === undefined) throw new Error('unexpected getUserMedia call');
				if (next instanceof Error) throw next;
				return next;
			}
		);
		vi.stubGlobal('navigator', { mediaDevices: { getUserMedia } });
		return getUserMedia;
	}

	const stream = { id: 'stream' } as MediaStream;

	it('falls back to the default devices when a listed one is gone', async () => {
		const getUserMedia = stubGetUserMedia(failure('OverconstrainedError'), stream);
		await expect(openCapture({ ...base, cameraId: 'gone', microphoneId: 'mic-1' })).resolves.toBe(
			stream
		);
		expect(getUserMedia).toHaveBeenCalledTimes(2);
		expect(getUserMedia.mock.calls[0][0]).toEqual(
			captureConstraints({ ...base, cameraId: 'gone', microphoneId: 'mic-1' })
		);
		expect(getUserMedia.mock.calls[1][0]).toEqual(captureConstraints(base));
	});

	it('goes on without audio when there is no microphone', async () => {
		const getUserMedia = stubGetUserMedia(failure('NotFoundError'), stream);
		await expect(openCapture(base)).resolves.toBe(stream);
		expect(getUserMedia.mock.calls[1][0]).toEqual(captureConstraints({ ...base, audio: false }));
	});

	it('throws when there is no camera either', async () => {
		stubGetUserMedia(failure('NotFoundError'), failure('NotFoundError'));
		await expect(openCapture(base)).rejects.toMatchObject({ name: 'NotFoundError' });
	});

	it('throws a denied permission without retrying', async () => {
		const getUserMedia = stubGetUserMedia(failure('NotAllowedError'));
		await expect(openCapture({ ...base, cameraId: 'cam-1' })).rejects.toMatchObject({
			name: 'NotAllowedError'
		});
		expect(getUserMedia).toHaveBeenCalledTimes(1);
	});

	it('does not retry an over-constrained request that named no device', async () => {
		const getUserMedia = stubGetUserMedia(failure('OverconstrainedError'));
		await expect(openCapture(base)).rejects.toMatchObject({ name: 'OverconstrainedError' });
		expect(getUserMedia).toHaveBeenCalledTimes(1);
	});
});

describe('describeCaptureError', () => {
	it('explains the errors the lobby can recover from', () => {
		expect(describeCaptureError({ name: 'NotAllowedError' })).toMatch(/blocked/);
		expect(describeCaptureError({ name: 'NotFoundError' })).toMatch(/No camera found/);
		expect(describeCaptureError({ name: 'NotReadableError' })).toMatch(/another app/i);
	});

	it('passes any other error through', () => {
		expect(describeCaptureError(new Error('boom'))).toBe('Could not open the camera: boom');
		expect(describeCaptureError('odd')).toBe('Could not open the camera: odd');
	});
});
