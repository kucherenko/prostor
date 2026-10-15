// The cameras and microphones the lobby offers, and what it remembers between visits:
// device ids and the display name in localStorage, never the producer secret and never
// the board. `pickDevice` and `toDeviceLists` are pure; the rest touches
// `navigator.mediaDevices` and `localStorage` and carries on when either is missing or
// blocked.

export interface DeviceOption {
	/** `MediaDeviceInfo.deviceId`; never empty. */
	id: string;
	/** What the picker shows: the device's label, or "Camera 1" while permission hides it. */
	label: string;
}

export interface DeviceLists {
	cameras: DeviceOption[];
	microphones: DeviceOption[];
}

type DeviceInfo = Pick<MediaDeviceInfo, 'kind' | 'deviceId' | 'label'>;

/** The id Chrome gives the audio device the operating system has selected. */
const DEFAULT_ID = 'default';

/**
 * The device to select: `preferredId` while it is still plugged in, otherwise the system
 * default, otherwise the first one listed (browsers list the default first). `null` when
 * there is no device at all.
 */
export function pickDevice(
	devices: readonly DeviceOption[],
	preferredId: string | null | undefined
): string | null {
	if (preferredId && devices.some((device) => device.id === preferredId)) return preferredId;
	return (devices.find((device) => device.id === DEFAULT_ID) ?? devices[0])?.id ?? null;
}

/**
 * Splits `enumerateDevices()` output into cameras and microphones. Until permission is
 * granted browsers return entries with an empty id and label; those are dropped, because
 * a device without an id cannot be selected.
 */
export function toDeviceLists(infos: readonly DeviceInfo[]): DeviceLists {
	const options = (kind: MediaDeviceKind, name: string): DeviceOption[] =>
		infos
			.filter((info) => info.kind === kind && info.deviceId !== '')
			.map((info, index) => ({ id: info.deviceId, label: info.label || `${name} ${index + 1}` }));
	return {
		cameras: options('videoinput', 'Camera'),
		microphones: options('audioinput', 'Microphone')
	};
}

/**
 * Lists the cameras and microphones. Call it after permission is granted (a successful
 * `getUserMedia`), or the ids and labels come back empty. Without the API, or if it fails,
 * the lists are empty and the lobby leaves the choice to the browser.
 */
export async function listDevices(): Promise<DeviceLists> {
	try {
		return toDeviceLists(await navigator.mediaDevices.enumerateDevices());
	} catch {
		return { cameras: [], microphones: [] };
	}
}

/** Calls `onChange` when a device is plugged in or removed. Returns the unsubscribe. */
export function watchDevices(onChange: () => void): () => void {
	const media = navigator.mediaDevices;
	if (!media) return () => undefined;
	media.addEventListener('devicechange', onChange);
	return () => media.removeEventListener('devicechange', onChange);
}

/** localStorage keys of the lobby's remembered choices. */
export const CAMERA_KEY = 'prostor.lobby.camera';
export const MICROPHONE_KEY = 'prostor.lobby.microphone';
export const NAME_KEY = 'prostor.lobby.name';

export interface LobbyPreferences {
	cameraId: string | null;
	microphoneId: string | null;
	displayName: string;
}

function read(key: string): string | null {
	try {
		return localStorage.getItem(key);
	} catch {
		return null;
	}
}

function write(key: string, value: string | null): void {
	try {
		if (value === null || value === '') localStorage.removeItem(key);
		else localStorage.setItem(key, value);
	} catch {
		// Private mode or blocked storage: the choices last for this page only.
	}
}

/** The remembered choices; anything missing or unreadable comes back empty. */
export function loadPreferences(): LobbyPreferences {
	return {
		cameraId: read(CAMERA_KEY) || null,
		microphoneId: read(MICROPHONE_KEY) || null,
		displayName: read(NAME_KEY) ?? ''
	};
}

/**
 * Remembers the given fields: a string stores it, `null` (or an empty name) forgets it,
 * and a field left out keeps what was saved.
 */
export function savePreferences(update: Partial<LobbyPreferences>): void {
	if (update.cameraId !== undefined) write(CAMERA_KEY, update.cameraId);
	if (update.microphoneId !== undefined) write(MICROPHONE_KEY, update.microphoneId);
	if (update.displayName !== undefined) write(NAME_KEY, update.displayName);
}
