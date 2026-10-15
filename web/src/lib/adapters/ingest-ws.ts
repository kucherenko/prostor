// The ingest WebSocket client, protocol v1 (`docs/protocol-ingest.md`): `hello`, one
// `u32 seq` + FLV tag per binary message, acks, `end_of_stream`. Applies the ADR 0003
// backpressure policy from `./backpressure` and reconnects when it says so. Runs in the
// compositor Worker so background tabs do not throttle it.

import type { IngestClientMessage } from '#lib/generated/IngestClientMessage';
import type { IngestServerMessage } from '#lib/generated/IngestServerMessage';
import type { StreamInfo } from '#lib/generated/StreamInfo';
import type { IngestPhase, IngestStatus } from '#lib/render/messages';
import {
	DEFAULT_BACKPRESSURE,
	admit,
	evaluate,
	initialState,
	type BackpressureActions,
	type BackpressureConfig,
	type BackpressureState
} from './backpressure';

/** The sequence number prefix of every binary message. */
export const SEQ_LEN = 4;
/** How long `hello` may wait for `started`, and `end_of_stream` for `ended`. */
export const HANDSHAKE_TIMEOUT_MS = 5000;
/** Reconnect backoff: first delay and cap. */
export const RECONNECT_MIN_MS = 1000;
export const RECONNECT_MAX_MS = 10_000;

export interface IngestTag {
	bytes: Uint8Array;
	kind: 'video' | 'audio';
	key: boolean;
	sequenceHeader: boolean;
}

export interface IngestClientOptions {
	url: string;
	auth: string | null;
	stream: StreamInfo;
	/** The encoder should produce a keyframe as soon as possible. */
	onKeyframeRequest: () => void;
	/** The encoder should switch to this bitrate. */
	onBitrate: (bitrate: number) => void;
	/** The session ended for a reason the client cannot recover from. */
	onFatal: (message: string) => void;
	backpressure?: BackpressureConfig;
	now?: () => number;
}

interface Outstanding {
	seq: number;
	sentAt: number;
}

/** Frames one tag as the protocol's binary message. */
export function encodeFrame(seq: number, tag: Uint8Array): Uint8Array<ArrayBuffer> {
	const message = new Uint8Array(new ArrayBuffer(SEQ_LEN + tag.byteLength));
	new DataView(message.buffer).setUint32(0, seq >>> 0, false);
	message.set(tag, SEQ_LEN);
	return message;
}

export class IngestClient {
	private socket: WebSocket | null = null;
	private phase: IngestPhase = 'idle';
	private seq = 0;
	private ackedSeq = 0;
	private readonly outstanding: Outstanding[] = [];
	private lastAckAt: number;
	private sentTags = 0;
	private sentBytes = 0;
	private droppedTags = 0;
	private reconnects = 0;
	private recording: string | null = null;
	private egress: IngestStatus['egress'] = [];
	private readonly policy: BackpressureState;
	private readonly config: BackpressureConfig;
	private readonly now: () => number;
	private videoHeader: Uint8Array | null = null;
	private audioHeader: Uint8Array | null = null;
	private readonly bytesHistory: { at: number; bytes: number }[] = [];
	private reconnectTimer: ReturnType<typeof setTimeout> | null = null;
	private reconnectDelay = RECONNECT_MIN_MS;
	private ending = false;

	constructor(private readonly opts: IngestClientOptions) {
		this.config = opts.backpressure ?? DEFAULT_BACKPRESSURE;
		this.now = opts.now ?? (() => performance.now());
		this.policy = initialState(opts.stream.video_bitrate, this.now());
		this.lastAckAt = this.now();
	}

	/** Opens the socket, completes `hello` → `started` and sends the remembered headers. */
	async connect(): Promise<{ recording: string | null }> {
		this.phase = 'connecting';
		const socket = new WebSocket(this.opts.url);
		socket.binaryType = 'arraybuffer';
		this.socket = socket;
		await new Promise<void>((resolve, reject) => {
			socket.onopen = () => resolve();
			socket.onerror = () => reject(new Error('ingest socket failed to open'));
			socket.onclose = (event) => reject(new Error(`ingest socket closed (${event.code})`));
		});
		const hello: IngestClientMessage = {
			type: 'hello',
			auth: this.opts.auth,
			stream: this.opts.stream
		};
		const started = this.waitFor('started', HANDSHAKE_TIMEOUT_MS);
		socket.send(JSON.stringify(hello));
		const message = await started;
		this.recording = message.type === 'started' ? message.recording : null;
		this.seq = 0;
		this.ackedSeq = 0;
		this.outstanding.length = 0;
		this.lastAckAt = this.now();
		this.phase = 'live';
		this.reconnectDelay = RECONNECT_MIN_MS;
		socket.onclose = (event) => this.onClose(event);
		socket.onerror = () => undefined;
		// Every server session opens with the sequence headers. `send` drops tags until
		// `started` but remembers headers, so any that arrived meanwhile go out now, and
		// video dropped meanwhile (or before a reconnect) resumes at a keyframe.
		for (const header of [this.videoHeader, this.audioHeader]) {
			if (header) this.sendRaw(header);
		}
		if (this.policy.dropping) this.opts.onKeyframeRequest();
		return { recording: this.recording };
	}

	/** Remembers the sequence headers so every new server session gets them first. */
	private remember(tag: IngestTag) {
		if (!tag.sequenceHeader) return;
		if (tag.kind === 'video') this.videoHeader = tag.bytes;
		else this.audioHeader = tag.bytes;
	}

	/** Applies the policy and sends the tag; returns whether it was sent. */
	send(tag: IngestTag): boolean {
		this.remember(tag);
		const socket = this.socket;
		// Only a server session that answered `started` takes media: the handshake, a
		// reconnect's backoff and a socket left open by a failed handshake all drop it,
		// and dropped video resumes at a keyframe.
		const live = this.phase === 'live' || this.phase === 'degraded';
		if (!socket || socket.readyState !== WebSocket.OPEN || !live) {
			if (tag.kind === 'video' && !tag.sequenceHeader) this.policy.dropping = true;
			this.droppedTags += 1;
			return false;
		}
		this.apply(this.evaluatePolicy());
		if (!admit(this.policy, tag)) {
			this.droppedTags += 1;
			return false;
		}
		const seq = ++this.seq;
		const message = encodeFrame(seq, tag.bytes);
		socket.send(message);
		this.outstanding.push({ seq, sentAt: this.now() });
		this.sentTags += 1;
		this.sentBytes += message.byteLength;
		return true;
	}

	/** Sends `end_of_stream` and waits for `ended`; resolves once the socket is closed. */
	async end(): Promise<void> {
		this.ending = true;
		if (this.reconnectTimer) clearTimeout(this.reconnectTimer);
		const socket = this.socket;
		if (!socket || socket.readyState !== WebSocket.OPEN) {
			this.phase = 'closed';
			return;
		}
		this.phase = 'ending';
		const message: IngestClientMessage = { type: 'end_of_stream', last_seq: this.seq };
		const ended = this.waitFor('ended', HANDSHAKE_TIMEOUT_MS).catch(() => undefined);
		socket.send(JSON.stringify(message));
		await ended;
		await this.closeSocket(1000, 'end of stream');
		this.phase = 'closed';
	}

	/** Current lag: the age of the oldest unacknowledged tag. */
	lagMs(): number {
		const oldest = this.outstanding[0];
		return oldest ? this.now() - oldest.sentAt : 0;
	}

	status(): IngestStatus {
		return {
			phase: this.phase,
			lagMs: Math.round(this.lagMs()),
			bufferedAmount: this.socket?.bufferedAmount ?? 0,
			sentTags: this.sentTags,
			sentBytes: this.sentBytes,
			droppedTags: this.droppedTags,
			ackedSeq: this.ackedSeq,
			lastSeq: this.seq,
			reconnects: this.reconnects,
			kbps: this.kbps(),
			recording: this.recording,
			egress: this.egress
		};
	}

	/** Re-evaluates the policy on a timer too, so a stalled link reacts without new tags. */
	tick(): void {
		if (this.phase === 'live' || this.phase === 'degraded') this.apply(this.evaluatePolicy());
		const now = this.now();
		this.bytesHistory.push({ at: now, bytes: this.sentBytes });
		while (this.bytesHistory.length > 1 && now - this.bytesHistory[0].at > 1000) {
			this.bytesHistory.shift();
		}
	}

	private kbps(): number {
		const first = this.bytesHistory[0];
		if (!first) return 0;
		const elapsed = this.now() - first.at;
		if (elapsed < 200) return 0;
		return Math.round(((this.sentBytes - first.bytes) * 8) / elapsed);
	}

	private evaluatePolicy(): BackpressureActions {
		const now = this.now();
		return evaluate(
			this.policy,
			{
				now,
				lagMs: this.lagMs(),
				outstanding: this.outstanding.length > 0,
				sinceAckMs: now - this.lastAckAt
			},
			this.config
		);
	}

	private apply(actions: BackpressureActions) {
		if (actions.requestKeyframe) this.opts.onKeyframeRequest();
		if (actions.setBitrate !== null) this.opts.onBitrate(actions.setBitrate);
		if (this.phase === 'live' || this.phase === 'degraded') {
			this.phase = actions.degraded ? 'degraded' : 'live';
		}
		if (actions.reconnect && !this.ending && this.phase !== 'reconnecting') {
			void this.closeSocket(4000, 'lag').then(() => this.scheduleReconnect('lag'));
		}
	}

	private onMessage(event: MessageEvent) {
		if (typeof event.data !== 'string') return;
		let message: IngestServerMessage;
		try {
			message = JSON.parse(event.data) as IngestServerMessage;
		} catch {
			return;
		}
		switch (message.type) {
			case 'ack': {
				this.lastAckAt = this.now();
				this.ackedSeq = Math.max(this.ackedSeq, message.seq);
				while (this.outstanding.length && this.outstanding[0].seq <= message.seq) {
					this.outstanding.shift();
				}
				break;
			}
			case 'egress_status':
				this.egress = message.egress;
				break;
			case 'error':
				this.opts.onFatal(`server: ${message.code}: ${message.message}`);
				break;
			case 'started':
			case 'ended':
				break;
		}
	}

	private waitFor(type: 'started' | 'ended', timeoutMs: number): Promise<IngestServerMessage> {
		const socket = this.socket;
		if (!socket) return Promise.reject(new Error('no socket'));
		return new Promise((resolve, reject) => {
			const timer = setTimeout(() => {
				socket.onmessage = (event) => this.onMessage(event);
				reject(new Error(`no ${type} within ${timeoutMs} ms`));
			}, timeoutMs);
			socket.onmessage = (event) => {
				this.onMessage(event);
				if (typeof event.data !== 'string') return;
				let message: IngestServerMessage;
				try {
					message = JSON.parse(event.data) as IngestServerMessage;
				} catch {
					return;
				}
				if (message.type === type || message.type === 'error') {
					clearTimeout(timer);
					socket.onmessage = (next) => this.onMessage(next);
					if (message.type === 'error') {
						reject(new Error(`server refused: ${message.code}: ${message.message}`));
					} else {
						resolve(message);
					}
				}
			};
		});
	}

	private onClose(event: CloseEvent) {
		if (this.ending || this.phase === 'closed') return;
		if (event.code >= 4000 || event.code === 1011) {
			this.phase = 'closed';
			this.opts.onFatal(`server closed the session (${event.code} ${event.reason})`);
			return;
		}
		this.scheduleReconnect(`closed (${event.code})`);
	}

	private closeSocket(code: number, reason: string): Promise<void> {
		const socket = this.socket;
		this.socket = null;
		if (!socket) return Promise.resolve();
		return new Promise((resolve) => {
			if (socket.readyState === WebSocket.CLOSED) return resolve();
			socket.onclose = () => resolve();
			socket.onerror = () => resolve();
			try {
				socket.close(code, reason);
			} catch {
				resolve();
			}
			setTimeout(resolve, 2000);
		});
	}

	private scheduleReconnect(why: string) {
		if (this.ending || this.phase === 'reconnecting') return;
		this.phase = 'reconnecting';
		this.outstanding.length = 0;
		this.policy.dropping = true;
		console.warn(`ingest: reconnecting after ${why} in ${this.reconnectDelay} ms`);
		this.reconnectTimer = setTimeout(() => void this.reconnect(), this.reconnectDelay);
		this.reconnectDelay = Math.min(RECONNECT_MAX_MS, this.reconnectDelay * 2);
	}

	private async reconnect(): Promise<void> {
		if (this.ending) return;
		this.reconnects += 1;
		try {
			// `scheduleReconnect` set video to drop until a keyframe, so `connect` resends
			// the sequence headers and asks the encoder for one.
			await this.connect();
		} catch (error) {
			if (this.ending) return;
			this.phase = 'idle';
			this.scheduleReconnect(error instanceof Error ? error.message : String(error));
		}
	}

	private sendRaw(bytes: Uint8Array) {
		const socket = this.socket;
		if (!socket || socket.readyState !== WebSocket.OPEN) return;
		const seq = ++this.seq;
		const message = encodeFrame(seq, bytes);
		socket.send(message);
		this.outstanding.push({ seq, sentAt: this.now() });
		this.sentTags += 1;
		this.sentBytes += message.byteLength;
	}
}
