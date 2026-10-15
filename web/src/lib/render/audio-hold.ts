// Audio that arrives before the video sequence header waits in a bounded queue. Only the
// first chunk of an encoder's output carries its decoder configuration (the AAC
// AudioSpecificConfig), so evicting it would leave the server without an audio sequence
// header for the whole session.

/**
 * Appends `chunk` to `held`, keeping at most `max` chunks. When the oldest chunk is evicted,
 * its decoder configuration moves to the new oldest one, so the audio sequence header still
 * goes out however long the video encoder takes to answer.
 */
export function holdAudio<T extends { description: unknown }>(
	held: T[],
	chunk: T,
	max: number
): void {
	held.push(chunk);
	if (held.length <= max) return;
	const evicted = held.shift() as T;
	const next = held[0];
	if (evicted.description && next && !next.description) {
		held[0] = { ...next, description: evicted.description };
	}
}
