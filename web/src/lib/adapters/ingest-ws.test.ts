import { afterEach, describe, expect, it, vi } from 'vitest';
import type { StreamInfo } from '#lib/generated/StreamInfo';
import {
	HANDSHAKE_TIMEOUT_MS,
	IngestClient,
	RECONNECT_MIN_MS,
	SEQ_LEN,
	encodeFrame,
	type IngestTag
} from './ingest-ws';

describe('encodeFrame', () => {
	it('prefixes the tag with a big-endian u32 sequence number', () => {
		const tag = new Uint8Array([9, 0, 0, 1, 0, 0, 0, 0, 0, 0, 0, 0x17, 0, 0, 0, 12]);
		const message = encodeFrame(0xdeadbeef, tag);
		expect(message.byteLength).toBe(SEQ_LEN + tag.byteLength);
		expect(Array.from(message.subarray(0, 4))).toEqual([0xde, 0xad, 0xbe, 0xef]);
		expect(Array.from(message.subarray(4))).toEqual(Array.from(tag));
	});

	it('starts counting at one and keeps the prefix for small numbers', () => {
		expect(Array.from(encodeFrame(1, new Uint8Array([7])))).toEqual([0, 0, 0, 1, 7]);
	});
});

/** Just enough of a WebSocket for the client: the test opens it and plays the server. */
class FakeSocket {
	static readonly CONNECTING = 0;
	static readonly OPEN = 1;
	static readonly CLOSING = 2;
	static readonly CLOSED = 3;
	static all: FakeSocket[] = [];
	readyState = FakeSocket.CONNECTING;
	binaryType = 'blob';
	bufferedAmount = 0;
	readonly sent: (string | Uint8Array)[] = [];
	onopen: (() => void) | null = null;
	onmessage: ((event: { data: unknown }) => void) | null = null;
	onerror: (() => void) | null = null;
	onclose: ((event: { code: number; reason: string }) => void) | null = null;

	constructor(readonly url: string) {
		FakeSocket.all.push(this);
	}

	send(data: string | Uint8Array) {
		this.sent.push(data);
	}

	close() {
		this.readyState = FakeSocket.CLOSED;
	}

	open() {
		this.readyState = FakeSocket.OPEN;
		this.onopen?.();
	}

	receive(message: object) {
		this.onmessage?.({ data: JSON.stringify(message) });
	}

	drop(code = 1006) {
		this.readyState = FakeSocket.CLOSED;
		this.onclose?.({ code, reason: '' });
	}

	binary(): Uint8Array[] {
		return this.sent.filter((message): message is Uint8Array => typeof message !== 'string');
	}
}

const STREAM: StreamInfo = {
	width: 1920,
	height: 1080,
	fps: 30,
	video_codec: 'avc1.4d0028',
	video_bitrate: 6_000_000,
	audio_codec: 'mp4a.40.2',
	audio_bitrate: 128_000,
	sample_rate: 48_000,
	channels: 2
};

const VIDEO_HEADER = new Uint8Array([9, 1]);
const AUDIO_HEADER = new Uint8Array([8, 1]);
const tags: Record<string, IngestTag> = {
	videoHeader: { bytes: VIDEO_HEADER, kind: 'video', key: true, sequenceHeader: true },
	keyframe: { bytes: new Uint8Array([9, 2]), kind: 'video', key: true, sequenceHeader: false },
	audioHeader: { bytes: AUDIO_HEADER, kind: 'audio', key: false, sequenceHeader: true },
	audio: { bytes: new Uint8Array([8, 2]), kind: 'audio', key: false, sequenceHeader: false }
};
const delta: IngestTag = {
	bytes: new Uint8Array([9, 3]),
	kind: 'video',
	key: false,
	sequenceHeader: false
};

function client(onKeyframeRequest = vi.fn()) {
	return new IngestClient({
		url: 'ws://studio/ingest',
		auth: null,
		stream: STREAM,
		onKeyframeRequest,
		onBitrate: () => undefined,
		onFatal: () => undefined
	});
}

/** Opens the newest socket and answers `hello` with `started`. */
async function handshake(): Promise<FakeSocket> {
	const socket = FakeSocket.all[FakeSocket.all.length - 1];
	socket.open();
	await vi.advanceTimersByTimeAsync(0);
	socket.receive({ type: 'started', room: 'studio', recording: null });
	await vi.advanceTimersByTimeAsync(0);
	return socket;
}

describe('IngestClient sequence headers', () => {
	afterEach(() => {
		FakeSocket.all = [];
		vi.useRealTimers();
		vi.unstubAllGlobals();
		vi.restoreAllMocks();
	});

	it('sends headers that arrived during the handshake first, then waits for a keyframe', async () => {
		vi.useFakeTimers();
		vi.stubGlobal('WebSocket', FakeSocket);
		const onKeyframeRequest = vi.fn();
		const ingest = client(onKeyframeRequest);
		const connected = ingest.connect();
		// The encoders answer before the server does: every tag is dropped, headers are kept.
		for (const tag of Object.values(tags)) expect(ingest.send(tag)).toBe(false);
		const socket = await handshake();
		await connected;
		expect(socket.binary()).toEqual([encodeFrame(1, VIDEO_HEADER), encodeFrame(2, AUDIO_HEADER)]);
		expect(onKeyframeRequest).toHaveBeenCalledOnce();
		// Video resumes at the keyframe the encoder was asked for, right after the headers.
		expect(ingest.send(delta)).toBe(false);
		expect(ingest.send(tags.keyframe)).toBe(true);
		expect(socket.binary()[2]).toEqual(encodeFrame(3, tags.keyframe.bytes));
	});

	it('sends nothing extra when the encoders answer after the handshake', async () => {
		vi.useFakeTimers();
		vi.stubGlobal('WebSocket', FakeSocket);
		const onKeyframeRequest = vi.fn();
		const ingest = client(onKeyframeRequest);
		const connected = ingest.connect();
		const socket = await handshake();
		await connected;
		expect(socket.binary()).toEqual([]);
		expect(onKeyframeRequest).not.toHaveBeenCalled();
		expect(ingest.send(tags.videoHeader)).toBe(true);
		expect(socket.binary()).toEqual([encodeFrame(1, VIDEO_HEADER)]);
	});

	it('starts a reconnected session with the headers and a keyframe request', async () => {
		vi.useFakeTimers();
		vi.stubGlobal('WebSocket', FakeSocket);
		vi.spyOn(console, 'warn').mockImplementation(() => undefined);
		const onKeyframeRequest = vi.fn();
		const ingest = client(onKeyframeRequest);
		const connected = ingest.connect();
		const first = await handshake();
		await connected;
		for (const tag of Object.values(tags)) expect(ingest.send(tag)).toBe(true);
		first.drop();
		await vi.advanceTimersByTimeAsync(RECONNECT_MIN_MS);
		const second = await handshake();
		expect(second).not.toBe(first);
		expect(second.binary()).toEqual([encodeFrame(1, VIDEO_HEADER), encodeFrame(2, AUDIO_HEADER)]);
		expect(onKeyframeRequest).toHaveBeenCalledOnce();
	});

	it('keeps media off a socket whose handshake failed, and still asks for a keyframe', async () => {
		vi.useFakeTimers();
		vi.stubGlobal('WebSocket', FakeSocket);
		vi.spyOn(console, 'warn').mockImplementation(() => undefined);
		const onKeyframeRequest = vi.fn();
		const ingest = client(onKeyframeRequest);
		const connected = ingest.connect();
		const first = await handshake();
		await connected;
		for (const tag of Object.values(tags)) expect(ingest.send(tag)).toBe(true);
		first.drop();
		await vi.advanceTimersByTimeAsync(RECONNECT_MIN_MS);
		// The reconnect's socket opens but the server never answers `started`.
		const halfOpen = FakeSocket.all[FakeSocket.all.length - 1];
		halfOpen.open();
		await vi.advanceTimersByTimeAsync(HANDSHAKE_TIMEOUT_MS);
		// Backing off: the half-open socket takes nothing, not even a keyframe.
		expect(ingest.send(tags.keyframe)).toBe(false);
		expect(ingest.send(tags.audio)).toBe(false);
		expect(halfOpen.binary()).toEqual([]);
		await vi.advanceTimersByTimeAsync(2 * RECONNECT_MIN_MS);
		const third = await handshake();
		expect(third).not.toBe(halfOpen);
		expect(third.binary()).toEqual([encodeFrame(1, VIDEO_HEADER), encodeFrame(2, AUDIO_HEADER)]);
		expect(onKeyframeRequest).toHaveBeenCalledOnce();
		expect(ingest.send(delta)).toBe(false);
		expect(ingest.send(tags.keyframe)).toBe(true);
	});
});
