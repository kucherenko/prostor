<script lang="ts" module>
	/** What the lobby hands over when the user joins. */
	export interface JoinedMedia {
		/** The open camera (and microphone) tracks; the receiver owns the stream and stops it. */
		stream: MediaStream;
		cameraId: string | null;
		/** `null` when the stream carries no audio. */
		microphoneId: string | null;
		displayName: string;
	}
</script>

<script lang="ts">
	// The pre-join screen: pick a camera and a microphone, check the self-view, enter a
	// display name. The producer uses it now and guests will from M5, so it holds only what
	// a guest needs too; what only the producer needs (room, secret) arrives as `children`.
	// The stream opened here is handed over on Join, so the browser asks for permission once.
	import { onMount, type Snippet } from 'svelte';
	import { CAMERA_CONSTRAINTS, describeCaptureError, openCapture } from '#lib/adapters/media';
	import {
		listDevices,
		loadPreferences,
		pickDevice,
		savePreferences,
		watchDevices,
		type DeviceOption
	} from '#lib/adapters/devices';

	interface Props {
		onJoin: (joined: JoinedMedia) => void;
		title?: string;
		/** Extra fields for the form, rendered between the display name and the Join button. */
		children?: Snippet;
	}

	let { onJoin, title = 'Get ready', children }: Props = $props();

	let cameras = $state<DeviceOption[]>([]);
	let microphones = $state<DeviceOption[]>([]);
	/** The selected devices; always one of the listed ids, or `null` when nothing is listed. */
	let cameraId = $state<string | null>(null);
	let microphoneId = $state<string | null>(null);
	let displayName = $state('');
	let stream = $state.raw<MediaStream | null>(null);
	let video = $state<HTMLVideoElement | null>(null);
	let opening = $state(true);
	let problem = $state<string | null>(null);
	let notice = $state<string | null>(null);

	/** Counts opens (and the teardown), so a slow `getUserMedia` never installs a stale stream. */
	let generation = 0;
	let destroyed = false;
	// Plain on purpose: a teardown reads `$state` as it was before the change that destroyed
	// the component, so it would still see the stream `submit` just handed over.
	let handedOver = false;

	onMount(() => {
		const stopWatching = watchDevices(() => void onDevicesChanged());
		void begin();
		return () => {
			destroyed = true;
			generation += 1;
			stopWatching();
			// A stream handed over on Join is no longer ours.
			if (!handedOver) release();
		};
	});

	// The self-view. The mirroring is CSS in the markup; the stream itself is never flipped.
	$effect(() => {
		if (!video) return;
		video.muted = true;
		video.srcObject = stream;
		if (stream) void video.play().catch(() => undefined);
	});

	async function refreshDevices() {
		const lists = await listDevices();
		cameras = lists.cameras;
		microphones = lists.microphones;
	}

	function release() {
		stream?.getTracks().forEach((track) => track.stop());
		stream = null;
	}

	/** The saved device when it is plugged in, otherwise the one in use. */
	function remembered(list: DeviceOption[], saved: string | null, current: string | null) {
		return saved && list.some((device) => device.id === saved) ? saved : current;
	}

	async function begin() {
		const saved = loadPreferences();
		displayName = saved.displayName;
		await refreshDevices();
		if (destroyed) return;
		cameraId = pickDevice(cameras, saved.cameraId);
		microphoneId = pickDevice(microphones, saved.microphoneId);
		// Until permission is granted browsers list no device ids, so this first open cannot
		// name a device and the browser picks.
		const blind = cameras.length === 0 || microphones.length === 0;
		await openStream();
		if (!blind || !stream) return;
		// The ids are listed now: switch once to the saved devices if they are not the open ones.
		const camera = remembered(cameras, saved.cameraId, cameraId);
		const microphone = remembered(microphones, saved.microphoneId, microphoneId);
		if (camera !== cameraId || microphone !== microphoneId) {
			cameraId = camera;
			microphoneId = microphone;
			await openStream();
		}
	}

	/** Opens the selected devices; the stream that was open is stopped first. */
	async function openStream(): Promise<void> {
		if (destroyed) return;
		const mine = ++generation;
		release();
		opening = true;
		problem = null;
		notice = null;
		try {
			// A device may have gone since the last listing (`ended` can arrive before
			// `devicechange`, or `devicechange` not at all): list again and ask only for devices
			// that are there, so `exact` never names an unplugged one. The selects follow.
			await refreshDevices();
			if (mine !== generation) return;
			cameraId = pickDevice(cameras, cameraId);
			microphoneId = pickDevice(microphones, microphoneId);
			const wantedCamera = cameraId;
			const wantedMicrophone = microphoneId;
			const next = await openCapture({
				...CAMERA_CONSTRAINTS,
				audio: true,
				cameraId: wantedCamera,
				microphoneId: wantedMicrophone
			});
			if (mine !== generation) {
				next.getTracks().forEach((track) => track.stop());
				return;
			}
			stream = next;
			const [videoTrack] = next.getVideoTracks();
			const [audioTrack] = next.getAudioTracks();
			videoTrack?.addEventListener('ended', () => {
				if (stream !== next) return;
				release();
				problem = 'The camera was disconnected.';
			});
			// Permission is granted now, so the lists carry ids and labels.
			await refreshDevices();
			if (mine !== generation) return;
			const usedCamera = videoTrack?.getSettings().deviceId;
			const usedMicrophone = audioTrack?.getSettings().deviceId;
			cameraId = pickDevice(cameras, usedCamera ?? cameraId);
			microphoneId = audioTrack ? pickDevice(microphones, usedMicrophone ?? microphoneId) : null;
			// An `exact` request opens that device or nothing, so another id means the open fell back
			// to the defaults (for both devices at once, see `openCapture`).
			const fellBack: string[] = [];
			if (wantedCamera && usedCamera && usedCamera !== wantedCamera) fellBack.push('camera');
			if (wantedMicrophone && usedMicrophone && usedMicrophone !== wantedMicrophone) {
				fellBack.push('microphone');
			}
			const notes: string[] = [];
			if (fellBack.length > 0) {
				const ones = fellBack.length > 1 ? 'ones' : 'one';
				notes.push(`Using the default ${fellBack.join(' and ')} instead of the selected ${ones}.`);
			}
			if (!audioTrack) notes.push('No microphone found: you will join without audio.');
			notice = notes.length > 0 ? notes.join(' ') : null;
		} catch (error) {
			if (mine !== generation) return;
			problem = describeCaptureError(error);
		} finally {
			if (mine === generation) opening = false;
		}
	}

	/** A device was plugged in or removed: refresh the lists, and move off one that is gone. */
	async function onDevicesChanged() {
		await refreshDevices();
		if (destroyed || opening) return;
		const camera = pickDevice(cameras, cameraId);
		const microphone = pickDevice(microphones, microphoneId);
		const moved = camera !== cameraId || microphone !== microphoneId;
		// The selection stays on listed devices even with nothing open (after an error), so
		// "Try again" never starts from a device that is gone.
		cameraId = camera;
		microphoneId = microphone;
		if (stream && moved) await openStream();
	}

	function chooseCamera(id: string) {
		cameraId = id;
		void openStream();
	}

	function chooseMicrophone(id: string) {
		microphoneId = id;
		void openStream();
	}

	function submit(event: SubmitEvent) {
		event.preventDefault();
		const name = displayName.trim();
		if (!stream || name === '') return;
		const joined: JoinedMedia = {
			stream,
			cameraId,
			microphoneId: stream.getAudioTracks().length > 0 ? microphoneId : null,
			displayName: name
		};
		// A choice that could not be resolved this time leaves the saved one alone.
		savePreferences({
			cameraId: joined.cameraId ?? undefined,
			microphoneId: joined.microphoneId ?? undefined,
			displayName: name
		});
		handedOver = true;
		stream = null;
		onJoin(joined);
	}
</script>

<section
	class="grid min-h-dvh place-items-center bg-gray-50 px-4 py-10 font-sans"
	data-testid="lobby"
>
	<form
		class="grid w-full max-w-4xl gap-6 rounded-lg border border-gray-200 bg-white p-6 shadow-sm md:grid-cols-[3fr_2fr]"
		onsubmit={submit}
	>
		<h1 class="text-2xl font-bold md:col-span-2">{title}</h1>

		<div class="relative">
			<!-- Mirrored like a mirror, for the person looking at it; the program is never mirrored. -->
			<video
				bind:this={video}
				class="aspect-video w-full -scale-x-100 rounded bg-black object-contain"
				autoplay
				muted
				playsinline
				aria-label="Camera preview"
				data-testid="lobby-preview"
			></video>
			{#if opening}
				<p class="absolute inset-0 grid place-items-center text-sm text-white/80" role="status">
					Opening camera…
				</p>
			{/if}
		</div>

		<div class="flex flex-col gap-3 text-sm">
			<label class="flex flex-col gap-1">
				<span>Camera</span>
				<select
					class="rounded border px-2 py-1"
					value={cameraId ?? ''}
					disabled={cameras.length === 0}
					onchange={(event) => chooseCamera(event.currentTarget.value)}
					data-testid="lobby-camera"
				>
					{#each cameras as camera (camera.id)}
						<option value={camera.id}>{camera.label}</option>
					{:else}
						<option value="">{opening ? 'Looking for cameras…' : 'No camera found'}</option>
					{/each}
				</select>
			</label>
			<label class="flex flex-col gap-1">
				<span>Microphone</span>
				<select
					class="rounded border px-2 py-1"
					value={microphoneId ?? ''}
					disabled={microphones.length === 0}
					onchange={(event) => chooseMicrophone(event.currentTarget.value)}
					data-testid="lobby-microphone"
				>
					{#each microphones as microphone (microphone.id)}
						<option value={microphone.id}>{microphone.label}</option>
					{:else}
						<option value="">{opening ? 'Looking for microphones…' : 'No microphone found'}</option>
					{/each}
				</select>
			</label>
			<label class="flex flex-col gap-1">
				<span>Display name</span>
				<input
					class="rounded border px-2 py-1"
					bind:value={displayName}
					required
					maxlength={40}
					autocomplete="off"
					placeholder="Your name"
					data-testid="lobby-name"
				/>
			</label>

			{@render children?.()}

			{#if problem}
				<div class="flex flex-col items-start gap-2 text-red-700" role="alert">
					<p data-testid="lobby-error">{problem}</p>
					<button
						type="button"
						class="rounded border border-current px-2 py-1"
						onclick={() => void openStream()}
						data-testid="lobby-retry">Try again</button
					>
				</div>
			{/if}
			{#if notice}
				<p class="text-amber-700" data-testid="lobby-notice">{notice}</p>
			{/if}

			<button
				type="submit"
				class="mt-1 rounded bg-gray-800 px-3 py-2 text-white disabled:opacity-40"
				disabled={!stream || displayName.trim() === ''}
				data-testid="lobby-join">Join</button
			>
		</div>
	</form>
</section>
