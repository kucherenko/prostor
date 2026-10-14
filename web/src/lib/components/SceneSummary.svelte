<script lang="ts">
	import type { LayerKind } from '#lib/generated/LayerKind';
	import type { Scene } from '#lib/generated/Scene';

	let { scene }: { scene: Scene } = $props();

	function describe(kind: LayerKind): string {
		switch (kind.type) {
			case 'camera':
				return 'Camera';
			case 'screen_share':
				return 'Screen share';
			case 'image':
				return `Image ${kind.src}`;
			case 'text':
				return `Text "${kind.text}"`;
			case 'color':
				return `Color rgba(${kind.rgba.join(', ')})`;
			case 'guest':
				return `Guest ${kind.participant_id}`;
		}
	}
</script>

<section class="mt-6 rounded border border-gray-200 p-4">
	<h2 class="text-xl font-semibold" data-testid="scene-name">{scene.name}</h2>
	<p class="text-sm text-gray-500">
		id {scene.id} · board position ({scene.position.x}, {scene.position.y})
	</p>
	<ul class="mt-3 space-y-1">
		{#each scene.layers as layer (layer.id)}
			<li class="flex justify-between font-mono text-sm" data-testid="layer">
				<span>{layer.name}: {describe(layer.kind)}</span>
				<span>{layer.rect.x},{layer.rect.y} {layer.rect.w}×{layer.rect.h} z{layer.z}</span>
			</li>
		{/each}
	</ul>
</section>
