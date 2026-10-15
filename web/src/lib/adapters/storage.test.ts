import { describe, expect, it } from 'vitest';
import { exportText, parseImport } from './storage';

describe('board import/export', () => {
	it('round-trips a document as pretty JSON', () => {
		const document = {
			schema_version: 1,
			board: {
				id: 'b',
				name: 'Show',
				scenes: [],
				transitions: [],
				participants: [],
				destinations: []
			}
		};
		const text = exportText(document);
		expect(text.endsWith('\n')).toBe(true);
		expect(text).toContain('"schema_version": 1');
		expect(parseImport(text)).toEqual(document);
	});

	it('refuses things that are not board documents', () => {
		expect(() => parseImport('42')).toThrow();
		expect(() => parseImport('{"board": {}}')).toThrow();
		expect(() => parseImport('{"schema_version": "1", "board": {}}')).toThrow();
		expect(() => parseImport('not json')).toThrow();
	});
});
