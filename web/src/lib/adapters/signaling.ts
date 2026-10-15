// The signaling socket client, protocol v1 (`docs/protocol-ingest.md`, "Signaling"):
// hello → welcome, then start_stream / stop_stream for the producer. Runs on the main
// thread; keys are sent once, in `start_stream`, and never stored here.

import type { DestinationSpec } from '#lib/generated/DestinationSpec';
import type { DestinationSummary } from '#lib/generated/DestinationSummary';
import type { PeerRole } from '#lib/generated/PeerRole';
import type { RoomClientMessage } from '#lib/generated/RoomClientMessage';
import type { RoomServerMessage } from '#lib/generated/RoomServerMessage';

export const SIGNALING_TIMEOUT_MS = 5000;

export interface SignalingOptions {
	url: string;
	auth: string | null;
	role: PeerRole;
	/** The socket closed while the client was still in use. */
	onClosed?: (code: number, reason: string) => void;
}

export class SignalingClient {
	private socket: WebSocket | null = null;
	private waiting: {
		types: RoomServerMessage['type'][];
		resolve: (m: RoomServerMessage) => void;
		reject: (e: Error) => void;
	}[] = [];
	private closedByUs = false;

	constructor(private readonly opts: SignalingOptions) {}

	/** Opens the socket and completes `hello` → `welcome`. */
	async connect(): Promise<void> {
		const socket = new WebSocket(this.opts.url);
		this.socket = socket;
		await new Promise<void>((resolve, reject) => {
			socket.onopen = () => resolve();
			socket.onerror = () => reject(new Error('signaling socket failed to open'));
			socket.onclose = (event) => reject(new Error(`signaling socket closed (${event.code})`));
		});
		socket.onmessage = (event) => this.onMessage(event);
		socket.onerror = () => undefined;
		socket.onclose = (event) => this.onClose(event);
		const hello: RoomClientMessage = { type: 'hello', auth: this.opts.auth, role: this.opts.role };
		const welcome = this.waitFor(['welcome']);
		socket.send(JSON.stringify(hello));
		await welcome;
	}

	/** Declares the destinations for the next ingest session. */
	async startStream(destinations: DestinationSpec[]): Promise<DestinationSummary[]> {
		const reply = this.waitFor(['stream_ready']);
		this.send({ type: 'start_stream', destinations });
		const message = await reply;
		return message.type === 'stream_ready' ? message.destinations : [];
	}

	/** Forgets the destinations on the server. */
	async stopStream(): Promise<void> {
		if (!this.socket || this.socket.readyState !== WebSocket.OPEN) return;
		const reply = this.waitFor(['stream_stopped']);
		this.send({ type: 'stop_stream' });
		await reply;
	}

	close(): void {
		this.closedByUs = true;
		this.socket?.close(1000, 'bye');
		this.socket = null;
	}

	get open(): boolean {
		return this.socket?.readyState === WebSocket.OPEN;
	}

	private send(message: RoomClientMessage) {
		if (!this.socket || this.socket.readyState !== WebSocket.OPEN) {
			throw new Error('signaling socket is not open');
		}
		this.socket.send(JSON.stringify(message));
	}

	private waitFor(types: RoomServerMessage['type'][]): Promise<RoomServerMessage> {
		return new Promise((resolve, reject) => {
			const entry = { types, resolve, reject };
			this.waiting.push(entry);
			setTimeout(() => {
				const index = this.waiting.indexOf(entry);
				if (index >= 0) {
					this.waiting.splice(index, 1);
					reject(new Error(`no ${types.join('/')} within ${SIGNALING_TIMEOUT_MS} ms`));
				}
			}, SIGNALING_TIMEOUT_MS);
		});
	}

	private onMessage(event: MessageEvent) {
		if (typeof event.data !== 'string') return;
		let message: RoomServerMessage;
		try {
			message = JSON.parse(event.data) as RoomServerMessage;
		} catch {
			return;
		}
		if (message.type === 'error') {
			// An error answers whatever is pending; `invalid_destination` keeps the socket.
			const pending = this.waiting.shift();
			pending?.reject(new Error(`${message.code}: ${message.message}`));
			return;
		}
		const index = this.waiting.findIndex((w) => w.types.includes(message.type));
		if (index >= 0) {
			const [waiter] = this.waiting.splice(index, 1);
			waiter.resolve(message);
		}
	}

	private onClose(event: CloseEvent) {
		for (const waiter of this.waiting.splice(0)) {
			waiter.reject(new Error(`signaling socket closed (${event.code} ${event.reason})`));
		}
		if (!this.closedByUs) this.opts.onClosed?.(event.code, event.reason);
		this.socket = null;
	}
}
