<script lang="ts">
	// The scene editor: a 16:9 canvas where layers are moved and resized, a layer list, and
	// the fields of the selected layer. Every change is a command to the store.
	import type { Layer } from '#lib/generated/Layer';
	import type { LayerKind } from '#lib/generated/LayerKind';
	import type { Rect } from '#lib/generated/Rect';
	import { PROGRAM_HEIGHT, PROGRAM_WIDTH, defaultLayer, type BoardStore } from './store.svelte';

	let { store }: { store: BoardStore } = $props();

	const scene = $derived(store.scene(store.selectedScene));
	const layers = $derived(scene ? [...scene.layers].sort((a, b) => a.z - b.z) : []);
	const selected = $derived(layers.find((l) => l.id === store.selectedLayer) ?? null);

	let svg = $state<SVGSVGElement | null>(null);
	let drag = $state<{
		id: string;
		mode: 'move' | 'resize';
		startX: number;
		startY: number;
		rect: Rect;
	} | null>(null);
	let preview = $state<Rect | null>(null);

	const colors: Record<LayerKind['type'], string> = {
		camera: '#2563eb',
		screen_share: '#7c3aed',
		image: '#059669',
		text: '#d97706',
		color: '#6b7280',
		guest: '#db2777'
	};

	function toProgram(event: PointerEvent): { x: number; y: number } {
		const rect = svg?.getBoundingClientRect();
		if (!rect) return { x: 0, y: 0 };
		return {
			x: ((event.clientX - rect.left) / rect.width) * PROGRAM_WIDTH,
			y: ((event.clientY - rect.top) / rect.height) * PROGRAM_HEIGHT
		};
	}

	function startDrag(event: PointerEvent, layer: Layer, mode: 'move' | 'resize') {
		if (event.button !== 0) return;
		event.stopPropagation();
		store.selectedLayer = layer.id;
		const p = toProgram(event);
		drag = { id: layer.id, mode, startX: p.x, startY: p.y, rect: { ...layer.rect } };
		preview = { ...layer.rect };
		svg?.setPointerCapture(event.pointerId);
	}

	function onMove(event: PointerEvent) {
		if (!drag) return;
		const p = toProgram(event);
		const dx = p.x - drag.startX;
		const dy = p.y - drag.startY;
		preview =
			drag.mode === 'move'
				? { ...drag.rect, x: drag.rect.x + dx, y: drag.rect.y + dy }
				: { ...drag.rect, w: Math.max(16, drag.rect.w + dx), h: Math.max(9, drag.rect.h + dy) };
	}

	function onUp() {
		if (drag && preview && scene) {
			const layer = scene.layers.find((l) => l.id === drag?.id);
			if (layer) {
				const rect = {
					x: Math.round(preview.x),
					y: Math.round(preview.y),
					w: Math.round(preview.w),
					h: Math.round(preview.h)
				};
				store.dispatch({ type: 'update_layer', scene: scene.id, layer: { ...layer, rect } });
			}
		}
		drag = null;
		preview = null;
	}

	function rectOf(layer: Layer): Rect {
		return drag?.id === layer.id && preview ? preview : layer.rect;
	}

	function addLayer(kind: LayerKind, name: string) {
		if (!scene) return;
		const z = scene.layers.reduce((m, l) => Math.max(m, l.z), -1) + 1;
		const layer = defaultLayer(kind, name, z);
		if (store.dispatch({ type: 'add_layer', scene: scene.id, layer }))
			store.selectedLayer = layer.id;
	}

	function update(patch: Partial<Layer>) {
		if (!scene || !selected) return;
		store.dispatch({ type: 'update_layer', scene: scene.id, layer: { ...selected, ...patch } });
	}

	function updateRect(field: keyof Rect, value: number) {
		if (!selected || !Number.isFinite(value)) return;
		update({ rect: { ...selected.rect, [field]: value } });
	}

	function updateCrop(field: keyof Rect, value: number) {
		if (!selected || !selected.crop || !Number.isFinite(value)) return;
		update({ crop: { ...selected.crop, [field]: value } });
	}

	function toggleCrop(on: boolean) {
		update({ crop: on ? { x: 0, y: 0, w: 1280, h: 720 } : null });
	}

	function removeLayer() {
		if (!scene || !selected) return;
		store.dispatch({ type: 'remove_layer', scene: scene.id, id: selected.id });
		store.selectedLayer = null;
	}

	/** Moves the layer one step up or down the stack by swapping z with its neighbour. */
	function restack(direction: 1 | -1) {
		if (!scene || !selected) return;
		const index = layers.findIndex((l) => l.id === selected.id);
		const neighbour = layers[index + direction];
		if (!neighbour) return;
		const a = {
			...selected,
			z: neighbour.z === selected.z ? neighbour.z + direction : neighbour.z
		};
		const b = { ...neighbour, z: selected.z };
		store.dispatch({ type: 'update_layer', scene: scene.id, layer: b });
		store.dispatch({ type: 'update_layer', scene: scene.id, layer: a });
	}

	function hex(rgba: [number, number, number, number]): string {
		return (
			'#' +
			rgba
				.slice(0, 3)
				.map((c) => c.toString(16).padStart(2, '0'))
				.join('')
		);
	}

	function fromHex(value: string, alpha: number): [number, number, number, number] {
		const n = parseInt(value.slice(1), 16);
		return [(n >> 16) & 255, (n >> 8) & 255, n & 255, alpha];
	}

	function kindLabel(kind: LayerKind): string {
		return kind.type.replace('_', ' ');
	}

	function numberInput(event: Event): number {
		return Number((event.currentTarget as HTMLInputElement).value);
	}
</script>

{#if scene}
	<div class="flex items-center gap-2">
		<input
			class="min-w-0 flex-1 rounded border px-2 py-1 font-semibold"
			value={scene.name}
			onchange={(e) =>
				store.dispatch({
					type: 'rename_scene',
					id: scene.id,
					name: (e.currentTarget as HTMLInputElement).value
				})}
			data-testid="scene-name-input"
		/>
		<button
			type="button"
			class="rounded bg-red-700 px-3 py-1 text-white disabled:opacity-40"
			disabled={store.live === scene.id}
			onclick={() => store.dispatch({ type: 'set_live', scene: scene.id })}
			data-testid="go-live">{store.live === scene.id ? 'On program' : 'Go live'}</button
		>
		<button
			type="button"
			class="rounded border px-3 py-1 text-gray-700"
			onclick={() => store.dispatch({ type: 'remove_scene', id: scene.id })}
			data-testid="delete-scene">Delete</button
		>
	</div>

	<svg
		bind:this={svg}
		viewBox="0 0 {PROGRAM_WIDTH} {PROGRAM_HEIGHT}"
		class="mt-2 aspect-video w-full touch-none rounded border border-gray-300 bg-black"
		role="application"
		aria-label="Scene editor"
		onpointermove={onMove}
		onpointerup={onUp}
		onpointercancel={onUp}
		onpointerdown={() => (store.selectedLayer = null)}
		data-testid="scene-editor"
	>
		{#each layers as layer (layer.id)}
			{@const r = rectOf(layer)}
			<g
				role="button"
				tabindex="-1"
				class="cursor-move"
				onpointerdown={(e) => startDrag(e, layer, 'move')}
				data-testid="editor-layer"
			>
				<rect
					x={r.x}
					y={r.y}
					width={r.w}
					height={r.h}
					fill={colors[layer.kind.type]}
					fill-opacity={layer.kind.type === 'color' ? layer.kind.rgba[3] / 255 : 0.35}
					stroke={selected?.id === layer.id ? 'white' : colors[layer.kind.type]}
					stroke-width={selected?.id === layer.id ? 8 : 3}
				/>
				<text x={r.x + 24} y={r.y + 60} class="fill-white" font-size="44" font-weight="600">
					{layer.name}
				</text>
				{#if selected?.id === layer.id}
					<rect
						x={r.x + r.w - 40}
						y={r.y + r.h - 40}
						width="40"
						height="40"
						fill="white"
						class="cursor-nwse-resize"
						role="button"
						tabindex="-1"
						aria-label="Resize"
						onpointerdown={(e) => startDrag(e, layer, 'resize')}
					/>
				{/if}
			</g>
		{/each}
	</svg>

	<div class="mt-2 flex flex-wrap gap-1 text-xs">
		<span class="self-center text-gray-500">Add:</span>
		<button
			type="button"
			class="rounded border px-2 py-1"
			onclick={() => addLayer({ type: 'camera' }, 'Camera')}>camera</button
		>
		<button
			type="button"
			class="rounded border px-2 py-1"
			onclick={() => addLayer({ type: 'screen_share' }, 'Screen')}>screen share</button
		>
		<button
			type="button"
			class="rounded border px-2 py-1"
			onclick={() => addLayer({ type: 'image', src: '/favicon.svg' }, 'Image')}>image</button
		>
		<button
			type="button"
			class="rounded border px-2 py-1"
			onclick={() => addLayer({ type: 'text', text: 'Title' }, 'Text')}>text</button
		>
		<button
			type="button"
			class="rounded border px-2 py-1"
			onclick={() => addLayer({ type: 'color', rgba: [30, 64, 175, 255] }, 'Color')}>color</button
		>
	</div>

	<ul class="mt-2 divide-y rounded border text-sm" data-testid="layer-list">
		{#each [...layers].reverse() as layer (layer.id)}
			<li>
				<button
					type="button"
					class="flex w-full items-center justify-between px-2 py-1 text-left {selected?.id ===
					layer.id
						? 'bg-blue-50'
						: ''}"
					onclick={() => (store.selectedLayer = layer.id)}
				>
					<span
						><span
							class="inline-block h-2 w-2 rounded-full"
							style="background: {colors[layer.kind.type]}"
						></span>
						{layer.name} <span class="text-gray-500">· {kindLabel(layer.kind)}</span></span
					>
					<span class="font-mono text-xs text-gray-500">z{layer.z}</span>
				</button>
			</li>
		{/each}
		{#if layers.length === 0}
			<li class="px-2 py-1 text-gray-500">No layers yet.</li>
		{/if}
	</ul>

	{#if selected}
		<div class="mt-2 grid grid-cols-2 gap-x-3 gap-y-1 text-xs" data-testid="layer-fields">
			<label class="col-span-2 flex items-center gap-2">
				<span class="w-12">Name</span>
				<input
					class="flex-1 rounded border px-2 py-1"
					value={selected.name}
					onchange={(e) => update({ name: (e.currentTarget as HTMLInputElement).value })}
				/>
			</label>
			{#if selected.kind.type === 'text'}
				<label class="col-span-2 flex items-center gap-2">
					<span class="w-12">Text</span>
					<input
						class="flex-1 rounded border px-2 py-1"
						value={selected.kind.text}
						onchange={(e) =>
							update({ kind: { type: 'text', text: (e.currentTarget as HTMLInputElement).value } })}
					/>
				</label>
			{:else if selected.kind.type === 'image'}
				<label class="col-span-2 flex items-center gap-2">
					<span class="w-12">URL</span>
					<input
						class="flex-1 rounded border px-2 py-1 font-mono"
						value={selected.kind.src}
						onchange={(e) =>
							update({ kind: { type: 'image', src: (e.currentTarget as HTMLInputElement).value } })}
					/>
				</label>
			{:else if selected.kind.type === 'color'}
				{@const rgba = selected.kind.rgba}
				<label class="flex items-center gap-2">
					<span class="w-12">Color</span>
					<input
						type="color"
						value={hex(rgba)}
						onchange={(e) =>
							update({
								kind: {
									type: 'color',
									rgba: fromHex((e.currentTarget as HTMLInputElement).value, rgba[3])
								}
							})}
					/>
				</label>
				<label class="flex items-center gap-2">
					<span class="w-12">Alpha</span>
					<input
						type="range"
						min="0"
						max="255"
						value={rgba[3]}
						onchange={(e) =>
							update({
								kind: { type: 'color', rgba: [rgba[0], rgba[1], rgba[2], numberInput(e)] }
							})}
					/>
				</label>
			{/if}
			{#each ['x', 'y', 'w', 'h'] as const as field (field)}
				<label class="flex items-center gap-2">
					<span class="w-12">{field}</span>
					<input
						type="number"
						class="w-24 rounded border px-2 py-1"
						value={Math.round(selected.rect[field])}
						onchange={(e) => updateRect(field, numberInput(e))}
						data-testid={`rect-${field}`}
					/>
				</label>
			{/each}
			<label class="col-span-2 flex items-center gap-2">
				<input
					type="checkbox"
					checked={selected.crop !== null}
					onchange={(e) => toggleCrop((e.currentTarget as HTMLInputElement).checked)}
				/>
				<span>Crop the source (source pixels)</span>
			</label>
			{#if selected.crop}
				{#each ['x', 'y', 'w', 'h'] as const as field (field)}
					<label class="flex items-center gap-2">
						<span class="w-12">crop {field}</span>
						<input
							type="number"
							class="w-24 rounded border px-2 py-1"
							value={Math.round(selected.crop[field])}
							onchange={(e) => updateCrop(field, numberInput(e))}
						/>
					</label>
				{/each}
			{/if}
			<div class="col-span-2 mt-1 flex gap-1">
				<button type="button" class="rounded border px-2 py-1" onclick={() => restack(1)}
					>Raise</button
				>
				<button type="button" class="rounded border px-2 py-1" onclick={() => restack(-1)}
					>Lower</button
				>
				<button
					type="button"
					class="ml-auto rounded border px-2 py-1 text-red-700"
					onclick={removeLayer}
					data-testid="remove-layer">Remove layer</button
				>
			</div>
		</div>
	{/if}
{:else}
	<p class="text-sm text-gray-500">Select a scene on the board to edit its layers.</p>
{/if}
