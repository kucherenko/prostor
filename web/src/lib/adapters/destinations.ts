// The producer's destinations, kept in this browser's localStorage. The key lives next
// to the URL here and nowhere else: it goes to the server once per session in
// `start_stream` and is never part of a board document or an export (M3 exports omit
// this store entirely).

import type { DestinationSpec } from '#lib/generated/DestinationSpec';

export const STORAGE_KEY = 'prostor.destinations.v1';

export interface StoredDestination {
	id: string;
	name: string;
	url: string;
	key: string;
	enabled: boolean;
}

export function newDestination(): StoredDestination {
	const id = `d-${Date.now().toString(36)}-${Math.random().toString(36).slice(2, 6)}`;
	return { id, name: '', url: '', key: '', enabled: true };
}

/** Parses a stored list, dropping anything that is not a destination. */
export function parseStored(text: string | null): StoredDestination[] {
	if (!text) return [];
	let value: unknown;
	try {
		value = JSON.parse(text);
	} catch {
		return [];
	}
	if (!Array.isArray(value)) return [];
	return value.flatMap((item) => {
		if (typeof item !== 'object' || item === null) return [];
		const d = item as Record<string, unknown>;
		if (typeof d.id !== 'string' || d.id.length === 0) return [];
		return [
			{
				id: d.id,
				name: typeof d.name === 'string' ? d.name : '',
				url: typeof d.url === 'string' ? d.url : '',
				key: typeof d.key === 'string' ? d.key : '',
				enabled: d.enabled !== false
			}
		];
	});
}

export function loadDestinations(): StoredDestination[] {
	try {
		return parseStored(localStorage.getItem(STORAGE_KEY));
	} catch {
		return [];
	}
}

export function saveDestinations(destinations: StoredDestination[]): void {
	try {
		localStorage.setItem(STORAGE_KEY, JSON.stringify(destinations));
	} catch {
		// Private mode or blocked storage: the list lives for this page only.
	}
}

/** The enabled, filled-in destinations as the protocol wants them. */
export function toSpecs(destinations: StoredDestination[]): DestinationSpec[] {
	return destinations
		.filter((d) => d.enabled && d.url.trim().length > 0 && d.key.length > 0)
		.map((d) => ({
			id: d.id,
			name: d.name.trim() || d.url.trim(),
			url: d.url.trim(),
			key: d.key
		}));
}

/** `rtmp://host/app/<masked>`-style display of a key: first and last two characters. */
export function maskKey(key: string): string {
	if (key.length === 0) return '';
	if (key.length <= 6) return '•'.repeat(key.length);
	return `${key.slice(0, 2)}${'•'.repeat(Math.min(12, key.length - 4))}${key.slice(-2)}`;
}
