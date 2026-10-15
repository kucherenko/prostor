<script lang="ts">
	import { onDestroy, onMount } from 'svelte';
	import { resolve } from '$app/paths';
	import Board from '#lib/board/Board.svelte';
	import SceneEditor from '#lib/board/SceneEditor.svelte';
	import { BoardStore } from '#lib/board/store.svelte';
	import { exportDocument, importDocument } from '#lib/adapters/storage';
	import {
		loadDestinations,
		maskKey,
		newDestination,
		saveDestinations,
		toSpecs,
		type StoredDestination
	} from '#lib/adapters/destinations';
	import { captureSupport } from '#lib/adapters/media';
	import { SignalingClient } from '#lib/adapters/signaling';
	import { emptyStatus, type PipelineStatus } from '#lib/render/messages';
	import { StudioPipeline } from '#lib/render/pipeline';

	const WIDTH = 1920;
	const HEIGHT = 1080;
	const FPS = 30;
	const SAMPLE_RATE = 48_000;

	type Phase = 'loading' | 'unsupported' | 'idle' | 'camera' | 'starting' | 'live' | 'stopping';

	let phase = $state<Phase>('loading');
	let room = $state('studio');
	let secret = $state('');
	let bitrateKbps = $state(4000);
	let withAudio = $state(true);
	let error = $state<string | null>(null);
	let missing = $state<string[]>([]);
	let recording = $state<string | null>(null);
	let audioActive = $state(false);
	let cameraSettings = $state<string | null>(null);
	let status = $state<PipelineStatus>(emptyStatus());
	let previewCanvas = $state<HTMLCanvasElement | null>(null);
	let destinations = $state<StoredDestination[]>([]);
	let store = $state<BoardStore | null>(null);
	let revealKeys = $state(false);
	let armed = $state<string[]>([]);

	let pipeline: StudioPipeline | null = null;
	let signaling: SignalingClient | null = null;

	$effect(() => {
		// Persist every edit (the key included: this browser is the only place it lives).
		saveDestinations(destinations);
	});

	onMount(async () => {
		destinations = loadDestinations();
		try {
			store = await BoardStore.open();
		} catch (cause) {
			error = `board: ${cause instanceof Error ? cause.message : String(cause)}`;
			phase = 'idle';
			return;
		}
		const support = captureSupport();
		if (!support.ok) {
			missing = support.missing;
			phase = 'unsupported';
			return;
		}
		try {
			const plan = store.renderPlan(WIDTH, HEIGHT);
			pipeline = new StudioPipeline({
				onStatus: (next) => (status = next),
				onStarted: () => undefined,
				onStopped: (last, reason) => {
					status = last;
					if (reason) error = reason;
					if (phase === 'live' || phase === 'starting' || phase === 'stopping') phase = 'camera';
				},
				onError: (message) => (error = message)
			});
			await pipeline.init(previewCanvas, plan);
			phase = 'idle';
		} catch (cause) {
			error = cause instanceof Error ? cause.message : String(cause);
			phase = 'idle';
		}
	});

	onDestroy(() => {
		signaling?.close();
		void pipeline?.dispose();
		void store?.flush();
	});

	// The compositor always draws what the board says is live.
	$effect(() => {
		if (!store || !pipeline) return;
		void store.doc;
		void store.live;
		pipeline.setPlan(store.renderPlan(WIDTH, HEIGHT));
	});

	function onKey(event: KeyboardEvent) {
		if (!store) return;
		const target = event.target as HTMLElement | null;
		if (target && ['INPUT', 'TEXTAREA', 'SELECT'].includes(target.tagName)) return;
		const digit = Number(event.key);
		if (Number.isInteger(digit) && digit >= 1 && digit <= 9) {
			const scene = store.scenes[digit - 1];
			if (scene) store.dispatch({ type: 'set_live', scene: scene.id });
		}
	}

	async function exportBoard() {
		if (!store) return;
		try {
			await exportDocument(store.doc);
		} catch (cause) {
			error = cause instanceof Error ? cause.message : String(cause);
		}
	}

	async function importBoard() {
		if (!store) return;
		try {
			const document = await importDocument();
			if (document) store.replace(document);
		} catch (cause) {
			error = `import: ${cause instanceof Error ? cause.message : String(cause)}`;
		}
	}

	function addDestination() {
		destinations = [...destinations, newDestination()];
	}

	function removeDestination(id: string) {
		destinations = destinations.filter((d) => d.id !== id);
	}

	function wsUrl(path: string): string {
		const scheme = location.protocol === 'https:' ? 'wss' : 'ws';
		return `${scheme}://${location.host}${path}`;
	}

	/** Opens the signaling socket and declares the enabled destinations. */
	async function arm(): Promise<void> {
		signaling?.close();
		const client = new SignalingClient({
			url: wsUrl(`/ws/room/${encodeURIComponent(room)}`),
			auth: secret.length > 0 ? secret : null,
			role: 'producer',
			onClosed: (code, reason) => {
				if (phase === 'live') error = `signaling socket closed (${code} ${reason})`;
			}
		});
		await client.connect();
		const ready = await client.startStream(toSpecs(destinations));
		armed = ready.map((d) => d.name);
		signaling = client;
	}

	async function openCamera() {
		if (!pipeline) return;
		error = null;
		try {
			const opened = await pipeline.openMedia({
				width: 1280,
				height: 720,
				fps: FPS,
				audio: withAudio,
				sampleRate: SAMPLE_RATE
			});
			const { width, height, frameRate } = opened.video;
			cameraSettings = `${width}×${height} @ ${frameRate ?? '?'} fps${opened.audio ? ', microphone' : ''}`;
			phase = 'camera';
		} catch (cause) {
			error = cause instanceof Error ? cause.message : String(cause);
		}
	}

	async function start() {
		if (!pipeline) return;
		error = null;
		recording = null;
		phase = 'starting';
		try {
			await arm();
			const started = await pipeline.start({
				url: wsUrl(`/ws/ingest/${encodeURIComponent(room)}`),
				auth: secret.length > 0 ? secret : null,
				width: WIDTH,
				height: HEIGHT,
				fps: FPS,
				videoBitrate: bitrateKbps * 1000,
				audio: withAudio,
				audioBitrate: 128_000,
				sampleRate: SAMPLE_RATE,
				channels: 2
			});
			recording = started.recording;
			audioActive = started.audio;
			phase = 'live';
		} catch (cause) {
			error = cause instanceof Error ? cause.message : String(cause);
			signaling?.close();
			signaling = null;
			armed = [];
			phase = 'camera';
		}
	}

	async function stop() {
		if (!pipeline) return;
		phase = 'stopping';
		try {
			status = await pipeline.stop();
			await signaling?.stopStream();
		} catch (cause) {
			error = cause instanceof Error ? cause.message : String(cause);
		}
		signaling?.close();
		signaling = null;
		armed = [];
		phase = 'camera';
	}

	const fmtK = (n: number) => (n >= 1000 ? `${(n / 1000).toFixed(1)}k` : String(n));
	const fmtMb = (bytes: number) => `${(bytes / 1_000_000).toFixed(1)} MB`;
</script>

<svelte:window onkeydown={onKey} />

<main class="mx-auto max-w-7xl p-6 font-sans">
	<header class="flex items-baseline justify-between gap-4">
		<h1 class="text-3xl font-bold">Studio</h1>
		{#if store}
			<input
				class="min-w-0 flex-1 rounded border px-2 py-1 text-lg"
				value={store.doc.board.name}
				onchange={(e) =>
					store?.dispatch({
						type: 'rename_board',
						name: (e.currentTarget as HTMLInputElement).value
					})}
				aria-label="Board name"
				data-testid="board-name"
			/>
			<button
				type="button"
				class="rounded border px-3 py-1 text-sm"
				onclick={exportBoard}
				data-testid="export">Export</button
			>
			<button
				type="button"
				class="rounded border px-3 py-1 text-sm"
				onclick={importBoard}
				data-testid="import">Import</button
			>
		{/if}
		<a class="text-blue-700 underline" href={resolve('/')}>Back</a>
	</header>

	{#if phase === 'unsupported'}
		<p class="mt-4 text-red-700" data-testid="error">
			This browser lacks {missing.join(', ')}. Use a current Chrome.
		</p>
	{/if}
	{#if store?.storageError}
		<p class="mt-2 text-sm text-amber-700" data-testid="storage-error">{store.storageError}</p>
	{/if}
	{#if store?.lastError}
		<p class="mt-2 text-sm text-red-700" data-testid="command-error">Refused: {store.lastError}</p>
	{/if}

	{#if store}
		<section class="mt-4 grid gap-4 lg:grid-cols-[3fr_2fr]">
			<div>
				<Board {store} />
			</div>
			<div data-testid="scene-panel">
				<SceneEditor {store} />
				{#if store.selectedTransition}
					{@const t = store.transitions.find((x) => x.id === store?.selectedTransition)}
					{#if t}
						<div class="mt-3 rounded border p-2 text-sm" data-testid="transition-panel">
							<div class="flex items-center justify-between">
								<span class="font-semibold">
									Transition {store.scene(t.from)?.name} → {store.scene(t.to)?.name}
								</span>
								<button
									type="button"
									class="rounded border px-2 py-1 text-red-700"
									onclick={() => store?.dispatch({ type: 'remove_transition', id: t.id })}
									data-testid="delete-transition">Delete</button
								>
							</div>
							<p class="mt-1 text-xs text-gray-500">
								{t.kind.type}{t.kind.type === 'cut' ? '' : `, ${t.duration_ms} ms`}. Crossfades and
								slides animate in M4; until then every switch is a cut.
							</p>
						</div>
					{/if}
				{/if}
			</div>
		</section>
	{/if}

	<h2 class="mt-6 flex items-center gap-3 text-lg font-semibold">
		Program
		<span class="text-sm font-normal text-gray-500" data-testid="program-scene">
			{store?.scene(store.live)?.name ?? 'nothing on program'}
		</span>
	</h2>

	<section class="mt-2 grid gap-4 md:grid-cols-[2fr_1fr]">
		<canvas
			bind:this={previewCanvas}
			class="aspect-video w-full rounded bg-black"
			width={WIDTH / 2}
			height={HEIGHT / 2}
			data-testid="preview"
		></canvas>

		<form class="flex flex-col gap-3 text-sm" onsubmit={(e) => e.preventDefault()}>
			<label class="flex flex-col gap-1">
				<span>Room</span>
				<input
					class="rounded border px-2 py-1"
					bind:value={room}
					disabled={phase !== 'idle' && phase !== 'camera'}
					data-testid="room"
					pattern={'[A-Za-z0-9_-]{1,64}'}
				/>
			</label>
			<label class="flex flex-col gap-1">
				<span>Producer secret (kept in memory only)</span>
				<input
					class="rounded border px-2 py-1"
					type="password"
					autocomplete="off"
					bind:value={secret}
					disabled={phase !== 'idle' && phase !== 'camera'}
					data-testid="secret"
				/>
			</label>
			<label class="flex flex-col gap-1">
				<span>Video bitrate</span>
				<select
					class="rounded border px-2 py-1"
					bind:value={bitrateKbps}
					disabled={phase === 'live'}
				>
					<option value={2500}>2.5 Mbit/s</option>
					<option value={4000}>4 Mbit/s</option>
					<option value={6000}>6 Mbit/s</option>
				</select>
			</label>
			<label class="flex items-center gap-2">
				<input type="checkbox" bind:checked={withAudio} disabled={phase !== 'idle'} />
				<span>Microphone (AAC-LC)</span>
			</label>
			<div class="mt-2 flex flex-wrap gap-2">
				<button
					type="button"
					class="rounded bg-gray-800 px-3 py-1 text-white disabled:opacity-40"
					onclick={openCamera}
					disabled={phase !== 'idle'}
					data-testid="open-camera">Open camera</button
				>
				<button
					type="button"
					class="rounded bg-red-700 px-3 py-1 text-white disabled:opacity-40"
					onclick={start}
					disabled={phase !== 'camera'}
					data-testid="start">Start</button
				>
				<button
					type="button"
					class="rounded bg-gray-500 px-3 py-1 text-white disabled:opacity-40"
					onclick={stop}
					disabled={phase !== 'live'}
					data-testid="stop">Stop</button
				>
			</div>
			<p class="text-gray-600">
				State: <span data-testid="phase">{phase}</span>
				{#if cameraSettings}· camera <span data-testid="camera">{cameraSettings}</span>{/if}
			</p>
			{#if recording}
				<p>
					Recording: <code data-testid="recording">{recording}</code>
					{audioActive ? '' : '(video only: no AAC encoder here)'}
				</p>
			{/if}
			{#if error}
				<p class="text-red-700" data-testid="error">{error}</p>
			{/if}
		</form>
	</section>

	<section class="mt-4 text-sm">
		<div class="flex items-baseline justify-between">
			<h2 class="text-lg font-semibold">Destinations</h2>
			<label class="flex items-center gap-2 text-xs text-gray-600">
				<input type="checkbox" bind:checked={revealKeys} />
				<span>Show keys</span>
			</label>
		</div>
		<p class="text-xs text-gray-600">
			URL without the key (e.g. <code>rtmp://live.twitch.tv/app</code>) and the key beside it.
			Stored in this browser only; sent to the server when you press Start, never written there.
		</p>
		<ul class="mt-2 flex flex-col gap-2" data-testid="destinations">
			{#each destinations as destination, i (destination.id)}
				<li class="grid grid-cols-[auto_1fr_2fr_2fr_auto] items-center gap-2">
					<input
						type="checkbox"
						bind:checked={destination.enabled}
						disabled={phase === 'live' || phase === 'starting'}
						title="Enabled"
					/>
					<input
						class="rounded border px-2 py-1"
						placeholder="Name"
						bind:value={destination.name}
						disabled={phase === 'live' || phase === 'starting'}
						data-testid={`dest-name-${i}`}
					/>
					<input
						class="rounded border px-2 py-1 font-mono"
						placeholder="rtmp://host/app"
						bind:value={destination.url}
						disabled={phase === 'live' || phase === 'starting'}
						data-testid={`dest-url-${i}`}
					/>
					{#if revealKeys}
						<input
							class="rounded border px-2 py-1 font-mono"
							placeholder="stream key"
							autocomplete="off"
							bind:value={destination.key}
							disabled={phase === 'live' || phase === 'starting'}
							data-testid={`dest-key-${i}`}
						/>
					{:else}
						<input
							class="rounded border px-2 py-1 font-mono"
							type="password"
							placeholder="stream key"
							autocomplete="off"
							bind:value={destination.key}
							disabled={phase === 'live' || phase === 'starting'}
							data-testid={`dest-key-${i}`}
							title={maskKey(destination.key)}
						/>
					{/if}
					<button
						type="button"
						class="rounded px-2 py-1 text-gray-600 hover:text-red-700 disabled:opacity-40"
						onclick={() => removeDestination(destination.id)}
						disabled={phase === 'live' || phase === 'starting'}
						title="Remove">✕</button
					>
				</li>
			{/each}
		</ul>
		<button
			type="button"
			class="mt-2 rounded border px-3 py-1 disabled:opacity-40"
			onclick={addDestination}
			disabled={phase === 'live' || phase === 'starting'}
			data-testid="dest-add">Add destination</button
		>
		{#if armed.length > 0}
			<p class="mt-1 text-xs text-gray-600" data-testid="armed">Armed: {armed.join(', ')}</p>
		{/if}
	</section>

	<section
		class="mt-4 grid grid-cols-2 gap-x-6 gap-y-1 font-mono text-xs sm:grid-cols-4 lg:grid-cols-6"
	>
		<span>fps</span><span data-testid="fps">{status.fps}</span>
		<span>rendered</span><span>{status.framesRendered}</span>
		<span>camera frames</span><span>{status.cameraFrames}</span>
		<span>encoder</span><span data-testid="encoder">{status.encoder.kind}</span>
		<span>encode queue</span><span>{status.encoder.queue}</span>
		<span>encoded / dropped</span>
		<span data-testid="encoded">{status.encoder.output} / {status.encoder.dropped}</span>
		<span>keyframes</span><span>{status.encoder.keyframes}</span>
		<span>bitrate</span>
		<span
			>{fmtK(status.encoder.bitrate / 1000)} / {fmtK(status.encoder.targetBitrate / 1000)} kbit/s</span
		>
		<span>audio chunks</span><span>{status.audio.enabled ? status.audio.chunks : 'off'}</span>
		<span>aac priming</span><span>{status.audio.primingMs} ms</span>
		<span>ingest</span><span data-testid="ingest-phase">{status.ingest.phase}</span>
		<span>lag</span><span data-testid="lag">{status.ingest.lagMs} ms</span>
		<span>bufferedAmount</span><span>{fmtK(status.ingest.bufferedAmount)}</span>
		<span>sent tags / dropped</span>
		<span data-testid="sent">{status.ingest.sentTags} / {status.ingest.droppedTags}</span>
		<span>sent</span><span>{fmtMb(status.ingest.sentBytes)} ({status.ingest.kbps} kbit/s)</span>
		<span>acked / last seq</span><span>{status.ingest.ackedSeq} / {status.ingest.lastSeq}</span>
		<span>reconnects</span><span>{status.ingest.reconnects}</span>
		{#each status.ingest.egress as egress (egress.id)}
			<span>egress {egress.id}</span>
			<span data-testid="egress" data-egress={egress.id}
				>{egress.state}
				{egress.tags} tags, {egress.dropped} dropped{egress.reconnects > 0
					? `, ${egress.reconnects} reconnects`
					: ''}{egress.detail ? ` (${egress.detail})` : ''}</span
			>
		{/each}
	</section>
</main>
