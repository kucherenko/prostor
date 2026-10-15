import { afterEach, describe, expect, it, vi } from 'vitest';
import {
	CAMERA_KEY,
	MICROPHONE_KEY,
	NAME_KEY,
	loadPreferences,
	pickDevice,
	savePreferences,
	toDeviceLists,
	type DeviceOption
} from './devices';

const original = Object.getOwnPropertyDescriptor(globalThis, 'localStorage');

afterEach(() => {
	vi.unstubAllGlobals();
	// Put back whatever the runtime had before a test replaced the accessor.
	if (original) Object.defineProperty(globalThis, 'localStorage', original);
	else Reflect.deleteProperty(globalThis, 'localStorage');
});

/** A working localStorage; `data` is what a test can inspect afterwards. */
function memoryStorage(initial: Record<string, string> = {}) {
	const data = new Map(Object.entries(initial));
	vi.stubGlobal('localStorage', {
		getItem: (key: string) => data.get(key) ?? null,
		setItem: (key: string, value: string) => void data.set(key, value),
		removeItem: (key: string) => void data.delete(key)
	});
	return data;
}

describe('pickDevice', () => {
	const devices: DeviceOption[] = [
		{ id: 'a', label: 'Built-in' },
		{ id: 'default', label: 'Default' },
		{ id: 'b', label: 'USB' }
	];

	it('keeps the preferred device while it is plugged in', () => {
		expect(pickDevice(devices, 'b')).toBe('b');
	});

	it('falls back to the system default when the preferred device is gone or none was saved', () => {
		expect(pickDevice(devices, 'unplugged')).toBe('default');
		expect(pickDevice(devices, null)).toBe('default');
		expect(pickDevice(devices, undefined)).toBe('default');
		expect(pickDevice(devices, '')).toBe('default');
	});

	it('falls back to the first device when there is no default entry', () => {
		const cameras: DeviceOption[] = [
			{ id: 'x', label: 'FaceTime' },
			{ id: 'y', label: 'Capture card' }
		];
		expect(pickDevice(cameras, 'unplugged')).toBe('x');
		expect(pickDevice(cameras, null)).toBe('x');
	});

	it('returns null for an empty list, whatever was preferred', () => {
		expect(pickDevice([], 'a')).toBeNull();
		expect(pickDevice([], null)).toBeNull();
	});
});

describe('toDeviceLists', () => {
	it('splits cameras from microphones and ignores other kinds', () => {
		const lists = toDeviceLists([
			{ kind: 'videoinput', deviceId: 'cam-1', label: 'FaceTime HD' },
			{ kind: 'audioinput', deviceId: 'default', label: 'Default - Mic' },
			{ kind: 'audiooutput', deviceId: 'spk', label: 'Speakers' },
			{ kind: 'audioinput', deviceId: 'mic-2', label: 'USB mic' }
		]);
		expect(lists.cameras).toEqual([{ id: 'cam-1', label: 'FaceTime HD' }]);
		expect(lists.microphones).toEqual([
			{ id: 'default', label: 'Default - Mic' },
			{ id: 'mic-2', label: 'USB mic' }
		]);
	});

	it('drops the id-less entries browsers return before permission is granted', () => {
		const lists = toDeviceLists([
			{ kind: 'videoinput', deviceId: '', label: '' },
			{ kind: 'audioinput', deviceId: '', label: '' }
		]);
		expect(lists).toEqual({ cameras: [], microphones: [] });
	});

	it('numbers devices whose label is still hidden', () => {
		const lists = toDeviceLists([
			{ kind: 'videoinput', deviceId: 'a', label: '' },
			{ kind: 'videoinput', deviceId: 'b', label: '' }
		]);
		expect(lists.cameras.map((c) => c.label)).toEqual(['Camera 1', 'Camera 2']);
	});
});

describe('lobby preferences', () => {
	it('starts empty', () => {
		memoryStorage();
		expect(loadPreferences()).toEqual({ cameraId: null, microphoneId: null, displayName: '' });
	});

	it('round-trips the choices under the lobby keys', () => {
		const data = memoryStorage();
		savePreferences({ cameraId: 'cam-1', microphoneId: 'mic-1', displayName: 'Ada' });
		expect(loadPreferences()).toEqual({
			cameraId: 'cam-1',
			microphoneId: 'mic-1',
			displayName: 'Ada'
		});
		expect([...data.keys()].sort()).toEqual([CAMERA_KEY, MICROPHONE_KEY, NAME_KEY].sort());
	});

	it('keeps what a partial save leaves out and forgets what is set to null', () => {
		const data = memoryStorage({
			[CAMERA_KEY]: 'cam-1',
			[MICROPHONE_KEY]: 'mic-1',
			[NAME_KEY]: 'Ada'
		});
		savePreferences({ displayName: 'Grace' });
		expect(loadPreferences()).toEqual({
			cameraId: 'cam-1',
			microphoneId: 'mic-1',
			displayName: 'Grace'
		});
		savePreferences({ microphoneId: null, displayName: '' });
		expect(loadPreferences()).toEqual({ cameraId: 'cam-1', microphoneId: null, displayName: '' });
		expect(data.has(MICROPHONE_KEY)).toBe(false);
		expect(data.has(NAME_KEY)).toBe(false);
	});

	it('survives a localStorage whose calls throw', () => {
		const blocked = () => {
			throw new Error('SecurityError: storage is blocked');
		};
		vi.stubGlobal('localStorage', { getItem: blocked, setItem: blocked, removeItem: blocked });
		expect(loadPreferences()).toEqual({ cameraId: null, microphoneId: null, displayName: '' });
		expect(() =>
			savePreferences({ cameraId: 'cam-1', microphoneId: null, displayName: 'Ada' })
		).not.toThrow();
	});

	it('survives a localStorage that throws on access, as Chrome does with cookies blocked', () => {
		Object.defineProperty(globalThis, 'localStorage', {
			configurable: true,
			get() {
				throw new Error('SecurityError: access is denied for this document');
			}
		});
		expect(loadPreferences()).toEqual({ cameraId: null, microphoneId: null, displayName: '' });
		expect(() => savePreferences({ cameraId: 'cam-1', displayName: 'Ada' })).not.toThrow();
	});
});
