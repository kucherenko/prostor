<script lang="ts">
	import './layout.css';
	import favicon from '#lib/assets/favicon.svg';
	import type { LayoutProps } from './$types';

	let { children }: LayoutProps = $props();
</script>

<svelte:head>
	<title>prostor</title>
	<link rel="icon" href={favicon} />
</svelte:head>

{@render children()}
