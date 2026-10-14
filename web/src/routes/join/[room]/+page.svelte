<script lang="ts">
	import { resolve } from '$app/paths';
	import { page } from '$app/state';
</script>

<main class="mx-auto max-w-2xl p-8 font-sans">
	<h1 class="text-3xl font-bold">Join room <span data-testid="room">{page.params.room}</span></h1>
	<p class="mt-2 text-gray-600">
		Guest view: camera and microphone check, then a WebRTC connection to the producer. Not built
		yet.
	</p>
	<a class="mt-8 inline-block text-blue-700 underline" href={resolve('/')}>Back</a>
</main>
