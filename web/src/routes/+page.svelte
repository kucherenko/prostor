<script lang="ts">
	import { onMount } from 'svelte';
	import { resolve } from '$app/paths';
	import SceneSummary from '#lib/components/SceneSummary.svelte';
	import type { Scene } from '#lib/generated/Scene';
	import { helloScene, wasmVersion } from '#lib/wasm';

	let scene = $state<Scene | null>(null);
	let version = $state<string | null>(null);
	let error = $state<string | null>(null);

	onMount(async () => {
		try {
			const [loadedScene, loadedVersion] = await Promise.all([helloScene(), wasmVersion()]);
			scene = loadedScene;
			version = loadedVersion;
		} catch (cause) {
			error = cause instanceof Error ? cause.message : String(cause);
		}
	});
</script>

<main class="mx-auto max-w-2xl p-8 font-sans">
	<h1 class="text-3xl font-bold">prostor</h1>
	<p class="mt-2 text-gray-600">
		Phase 0 skeleton. The scene below is built by the Rust core running as WebAssembly and rendered
		through the generated TypeScript types.
	</p>

	{#if error}
		<p class="mt-4 text-red-700" data-testid="error">Failed to load the Rust core: {error}</p>
	{:else if scene}
		<p class="mt-4 text-sm text-gray-500">
			prostor-wasm <span data-testid="wasm-version">{version}</span>
		</p>
		<SceneSummary {scene} />
	{:else}
		<p class="mt-4 text-gray-500">Loading the Rust core…</p>
	{/if}

	<nav class="mt-8 flex gap-4 text-blue-700 underline">
		<a href={resolve('/studio')}>Studio</a>
		<a href={resolve('/join/[room]', { room: 'demo' })}>Join (demo room)</a>
	</nav>
</main>
