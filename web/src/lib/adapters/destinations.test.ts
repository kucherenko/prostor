import { describe, expect, it } from 'vitest';
import { maskKey, parseStored, toSpecs, type StoredDestination } from './destinations';

describe('destinations store', () => {
	it('parses stored lists defensively', () => {
		expect(parseStored(null)).toEqual([]);
		expect(parseStored('nonsense')).toEqual([]);
		expect(parseStored('{"a":1}')).toEqual([]);
		expect(
			parseStored(
				JSON.stringify([
					{ id: 'a', name: 'Twitch', url: 'rtmp://x/app', key: 'k', enabled: false },
					{ id: 'b', url: 'rtmp://y/app' },
					{ name: 'no id' },
					42
				])
			)
		).toEqual([
			{ id: 'a', name: 'Twitch', url: 'rtmp://x/app', key: 'k', enabled: false },
			{ id: 'b', name: '', url: 'rtmp://y/app', key: '', enabled: true }
		]);
	});

	it('sends only enabled, complete destinations', () => {
		const stored: StoredDestination[] = [
			{ id: 'a', name: ' Twitch ', url: ' rtmp://x/app ', key: 'k1', enabled: true },
			{ id: 'b', name: '', url: 'rtmp://y/app', key: 'k2', enabled: true },
			{ id: 'c', name: 'off', url: 'rtmp://z/app', key: 'k3', enabled: false },
			{ id: 'd', name: 'no key', url: 'rtmp://z/app', key: '', enabled: true },
			{ id: 'e', name: 'no url', url: '', key: 'k', enabled: true }
		];
		expect(toSpecs(stored)).toEqual([
			{ id: 'a', name: 'Twitch', url: 'rtmp://x/app', key: 'k1' },
			{ id: 'b', name: 'rtmp://y/app', url: 'rtmp://y/app', key: 'k2' }
		]);
	});

	it('masks keys', () => {
		expect(maskKey('')).toBe('');
		expect(maskKey('abc')).toBe('•••');
		expect(maskKey('live_1234567890_abcdef')).toBe('li••••••••••••ef');
	});
});
