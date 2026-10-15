<script lang="ts">
	import { onDestroy, onMount } from 'svelte';
	import { resolve } from '$app/paths';
	import type { RenderPlan } from '#lib/generated/RenderPlan';
	import { captureSupport } from '#lib/adapters/media';
	import { emptyStatus, type PipelineStatus } from '#lib/render/messages';
	import { StudioPipeline } from '#lib/render/pipeline';
	import { helloScene, loadWasm } from '#lib/wasm';
	import { render_plan } from '#lib/wasm/pkg/prostor_wasm.js';

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

	let pipeline: StudioPipeline | null = null;

	onMount(async () => {
		const support = captureSupport();
		if (!support.ok) {
			missing = support.missing;
			phase = 'unsupported';
			return;
		}
		try {
			await loadWasm();
			const plan = render_plan(await helloScene(), WIDTH, HEIGHT) as RenderPlan;
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
		void pipeline?.dispose();
	});

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

	function ingestUrl(): string {
		const scheme = location.protocol === 'https:' ? 'wss' : 'ws';
		return `${scheme}://${location.host}/ws/ingest/${encodeURIComponent(room)}`;
	}

	async function start() {
		if (!pipeline) return;
		error = null;
		recording = null;
		phase = 'starting';
		try {
			const started = await pipeline.start({
				url: ingestUrl(),
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
			phase = 'camera';
		}
	}

	async function stop() {
		if (!pipeline) return;
		phase = 'stopping';
		try {
			status = await pipeline.stop();
		} catch (cause) {
			error = cause instanceof Error ? cause.message : String(cause);
		}
		phase = 'camera';
	}

	const fmtK = (n: number) => (n >= 1000 ? `${(n / 1000).toFixed(1)}k` : String(n));
	const fmtMb = (bytes: number) => `${(bytes / 1_000_000).toFixed(1)} MB`;
</script>

<main class="mx-auto max-w-5xl p-6 font-sans">
	<header class="flex items-baseline justify-between">
		<h1 class="text-3xl font-bold">Studio</h1>
		<a class="text-blue-700 underline" href={resolve('/')}>Back</a>
	</header>
	<p class="mt-1 text-sm text-gray-600">
		Tracer bullet: camera + title → WebGL2 compositor → H.264/AAC → FLV → the server's recording.
	</p>

	{#if phase === 'unsupported'}
		<p class="mt-4 text-red-700" data-testid="error">
			This browser lacks {missing.join(', ')}. Use a current Chrome.
		</p>
	{/if}

	<section class="mt-4 grid gap-4 md:grid-cols-[2fr_1fr]">
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
			<span data-testid="egress">{egress.state} {egress.tags} tags, {egress.dropped} dropped</span>
		{/each}
	</section>
</main>
