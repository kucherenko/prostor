<script lang="ts">
	// The infinite board: scene nodes on a pannable, zoomable SVG canvas, joined by
	// transition edges. Click selects a scene, double-click puts it on program, dragging the
	// handle on a node's right edge onto another node adds a transition.
	import type { Transition } from '#lib/generated/Transition';
	import { newId, type BoardStore } from './store.svelte';

	let { store }: { store: BoardStore } = $props();

	const NODE_W = 200;
	const NODE_H = 112;

	let svg = $state<SVGSVGElement | null>(null);
	let pan = $state({ x: 40, y: 40 });
	let zoom = $state(1);
	let drag = $state<
		| { kind: 'pan'; startX: number; startY: number; panX: number; panY: number }
		| { kind: 'node'; id: string; offsetX: number; offsetY: number; moved: boolean }
		| { kind: 'edge'; from: string; x: number; y: number }
		| null
	>(null);
	/** Node positions overridden while dragging, so the engine is told once at the end. */
	let dragPosition = $state<{ id: string; x: number; y: number } | null>(null);
	let hoverTarget = $state<string | null>(null);

	function position(id: string): { x: number; y: number } {
		if (dragPosition?.id === id) return dragPosition;
		const scene = store.scene(id);
		return scene ? scene.position : { x: 0, y: 0 };
	}

	/** Board coordinates of a pointer event. */
	function toBoard(event: PointerEvent): { x: number; y: number } {
		const rect = svg?.getBoundingClientRect();
		const px = event.clientX - (rect?.left ?? 0);
		const py = event.clientY - (rect?.top ?? 0);
		return { x: (px - pan.x) / zoom, y: (py - pan.y) / zoom };
	}

	function onBackgroundDown(event: PointerEvent) {
		if (event.button !== 0) return;
		store.selectedTransition = null;
		drag = { kind: 'pan', startX: event.clientX, startY: event.clientY, panX: pan.x, panY: pan.y };
		(event.currentTarget as Element).setPointerCapture(event.pointerId);
	}

	function onNodeDown(event: PointerEvent, id: string) {
		if (event.button !== 0) return;
		event.stopPropagation();
		const p = toBoard(event);
		const node = position(id);
		drag = { kind: 'node', id, offsetX: p.x - node.x, offsetY: p.y - node.y, moved: false };
		store.selectedScene = id;
		store.selectedLayer = null;
		store.selectedTransition = null;
		svg?.setPointerCapture(event.pointerId);
	}

	function onHandleDown(event: PointerEvent, from: string) {
		if (event.button !== 0) return;
		event.stopPropagation();
		const p = toBoard(event);
		drag = { kind: 'edge', from, x: p.x, y: p.y };
		svg?.setPointerCapture(event.pointerId);
	}

	function onMove(event: PointerEvent) {
		if (!drag) return;
		if (drag.kind === 'pan') {
			pan = {
				x: drag.panX + (event.clientX - drag.startX),
				y: drag.panY + (event.clientY - drag.startY)
			};
		} else if (drag.kind === 'node') {
			const p = toBoard(event);
			dragPosition = { id: drag.id, x: p.x - drag.offsetX, y: p.y - drag.offsetY };
			drag.moved = true;
		} else {
			const p = toBoard(event);
			drag = { ...drag, x: p.x, y: p.y };
			hoverTarget = nodeAt(p.x, p.y, drag.from);
		}
	}

	function onUp() {
		if (!drag) return;
		if (drag.kind === 'node' && drag.moved && dragPosition) {
			store.dispatch({
				type: 'move_scene',
				id: drag.id,
				position: { x: Math.round(dragPosition.x), y: Math.round(dragPosition.y) }
			});
		} else if (drag.kind === 'edge' && hoverTarget) {
			const transition: Transition = {
				id: newId('transition'),
				from: drag.from,
				to: hoverTarget,
				kind: { type: 'cut' },
				duration_ms: 0
			};
			if (store.dispatch({ type: 'add_transition', transition })) {
				store.selectedTransition = transition.id;
			}
		}
		drag = null;
		dragPosition = null;
		hoverTarget = null;
	}

	function nodeAt(x: number, y: number, except: string): string | null {
		for (const scene of store.scenes) {
			if (scene.id === except) continue;
			const p = position(scene.id);
			if (x >= p.x && x <= p.x + NODE_W && y >= p.y && y <= p.y + NODE_H) return scene.id;
		}
		return null;
	}

	function onWheel(event: WheelEvent) {
		event.preventDefault();
		const rect = svg?.getBoundingClientRect();
		const px = event.clientX - (rect?.left ?? 0);
		const py = event.clientY - (rect?.top ?? 0);
		const factor = Math.exp(-event.deltaY * 0.0015);
		const next = Math.min(3, Math.max(0.25, zoom * factor));
		// Keep the point under the cursor fixed.
		pan = { x: px - ((px - pan.x) * next) / zoom, y: py - ((py - pan.y) * next) / zoom };
		zoom = next;
	}

	function addScene() {
		const rect = svg?.getBoundingClientRect();
		const center = {
			x: ((rect?.width ?? 600) / 2 - pan.x) / zoom - NODE_W / 2,
			y: ((rect?.height ?? 400) / 2 - pan.y) / zoom - NODE_H / 2
		};
		const id = newId('scene');
		const n = store.scenes.length + 1;
		if (
			store.dispatch({
				type: 'add_scene',
				id,
				name: `Scene ${n}`,
				position: { x: Math.round(center.x + (n % 3) * 24), y: Math.round(center.y + (n % 3) * 24) }
			})
		) {
			store.selectedScene = id;
			store.selectedTransition = null;
		}
	}

	function edgePath(t: Transition): string {
		const a = position(t.from);
		const b = position(t.to);
		const x1 = a.x + NODE_W;
		const y1 = a.y + NODE_H / 2;
		const x2 = b.x;
		const y2 = b.y + NODE_H / 2;
		const dx = Math.max(40, Math.abs(x2 - x1) / 2);
		return `M ${x1} ${y1} C ${x1 + dx} ${y1}, ${x2 - dx} ${y2}, ${x2} ${y2}`;
	}

	function kindLabel(t: Transition): string {
		if (t.kind.type === 'cut') return 'cut';
		if (t.kind.type === 'crossfade') return `fade ${t.duration_ms} ms`;
		return `slide ${t.kind.direction} ${t.duration_ms} ms`;
	}

	const selected = $derived(store.selectedScene);
	const live = $derived(store.live);
	const selectedTransition = $derived(store.selectedTransition);
</script>

<div class="flex items-center gap-2 text-sm">
	<button
		type="button"
		class="rounded bg-gray-800 px-3 py-1 text-white"
		onclick={addScene}
		data-testid="add-scene">Add scene</button
	>
	<span class="text-xs text-gray-500">
		Drag to pan, wheel to zoom. Click a scene to edit it, double-click to put it on program, drag
		the handle to another scene to add a transition. Keys 1–9 switch the program.
	</span>
</div>

<svg
	bind:this={svg}
	class="mt-2 h-[420px] w-full touch-none rounded border border-gray-300 bg-gray-50"
	role="application"
	aria-label="Board"
	onpointerdown={onBackgroundDown}
	onpointermove={onMove}
	onpointerup={onUp}
	onpointercancel={onUp}
	onwheel={onWheel}
	data-testid="board"
>
	<defs>
		<marker
			id="arrow"
			viewBox="0 0 10 10"
			refX="9"
			refY="5"
			markerWidth="8"
			markerHeight="8"
			orient="auto-start-reverse"
		>
			<path d="M 0 0 L 10 5 L 0 10 z" fill="#6b7280" />
		</marker>
		<pattern
			id="grid"
			width={40 * zoom}
			height={40 * zoom}
			patternUnits="userSpaceOnUse"
			x={pan.x}
			y={pan.y}
		>
			<circle cx="1" cy="1" r="1" fill="#d1d5db" />
		</pattern>
	</defs>
	<rect width="100%" height="100%" fill="url(#grid)" />
	<g transform="translate({pan.x} {pan.y}) scale({zoom})">
		{#each store.transitions as t (t.id)}
			<g
				role="button"
				tabindex="-1"
				onpointerdown={(e) => {
					e.stopPropagation();
					store.selectedTransition = t.id;
				}}
				data-testid="edge"
			>
				<path d={edgePath(t)} fill="none" stroke="transparent" stroke-width="16" />
				<path
					d={edgePath(t)}
					fill="none"
					stroke={selectedTransition === t.id ? '#2563eb' : '#6b7280'}
					stroke-width={selectedTransition === t.id ? 3 : 2}
					marker-end="url(#arrow)"
				/>
				<text
					x={(position(t.from).x + NODE_W + position(t.to).x) / 2}
					y={(position(t.from).y + position(t.to).y) / 2 + NODE_H / 2 - 8}
					class="fill-gray-600 text-[11px]"
					text-anchor="middle">{kindLabel(t)}</text
				>
			</g>
		{/each}
		{#if drag?.kind === 'edge'}
			{@const from = position(drag.from)}
			<line
				x1={from.x + NODE_W}
				y1={from.y + NODE_H / 2}
				x2={drag.x}
				y2={drag.y}
				stroke="#2563eb"
				stroke-width="2"
				stroke-dasharray="6 4"
			/>
		{/if}
		{#each store.scenes as scene, i (scene.id)}
			{@const p = position(scene.id)}
			<g
				transform="translate({p.x} {p.y})"
				role="button"
				tabindex="-1"
				class="cursor-move select-none"
				onpointerdown={(e) => onNodeDown(e, scene.id)}
				ondblclick={() => store.dispatch({ type: 'set_live', scene: scene.id })}
				data-testid="scene-node"
				data-scene={scene.id}
			>
				<rect
					width={NODE_W}
					height={NODE_H}
					rx="10"
					fill={hoverTarget === scene.id ? '#dbeafe' : live === scene.id ? '#fef2f2' : 'white'}
					stroke={selected === scene.id ? '#2563eb' : live === scene.id ? '#dc2626' : '#9ca3af'}
					stroke-width={selected === scene.id ? 3 : 2}
				/>
				<text x="12" y="24" class="fill-gray-800 text-[14px] font-semibold"
					>{i + 1}. {scene.name}</text
				>
				<text x="12" y="46" class="fill-gray-500 text-[11px]">
					{scene.layers.length} layer{scene.layers.length === 1 ? '' : 's'}
				</text>
				{#each scene.layers.slice(0, 4) as layer, j (layer.id)}
					<text x="12" y={64 + j * 13} class="fill-gray-600 text-[10px]"
						>{layer.name} · {layer.kind.type}</text
					>
				{/each}
				{#if live === scene.id}
					<rect x={NODE_W - 52} y="8" width="44" height="18" rx="4" fill="#dc2626" />
					<text x={NODE_W - 30} y="21" class="fill-white text-[10px] font-bold" text-anchor="middle"
						>LIVE</text
					>
				{/if}
				<circle
					cx={NODE_W}
					cy={NODE_H / 2}
					r="8"
					fill="#2563eb"
					class="cursor-crosshair"
					role="button"
					tabindex="-1"
					aria-label="Drag to add a transition"
					onpointerdown={(e) => onHandleDown(e, scene.id)}
					data-testid="edge-handle"
				/>
			</g>
		{/each}
	</g>
</svg>
