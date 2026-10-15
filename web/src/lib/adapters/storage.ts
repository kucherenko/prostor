// Board persistence in this browser (IndexedDB) and import/export as JSON files through
// the File System Access API, with download/upload fallbacks. The document never holds
// stream keys (they live in `destinations.ts`), so an export is safe to share.

import type { BoardDocument } from '#lib/generated/BoardDocument';

export const DB_NAME = 'prostor';
export const DB_VERSION = 1;
export const STORE = 'boards';
/** localStorage key of the board id to open on load. */
export const CURRENT_BOARD_KEY = 'prostor.board.current';

function openDb(): Promise<IDBDatabase> {
	return new Promise((resolve, reject) => {
		const request = indexedDB.open(DB_NAME, DB_VERSION);
		request.onupgradeneeded = () => {
			const db = request.result;
			if (!db.objectStoreNames.contains(STORE)) db.createObjectStore(STORE);
		};
		request.onsuccess = () => resolve(request.result);
		request.onerror = () => reject(request.error ?? new Error('indexedDB open failed'));
	});
}

function requestToPromise<T>(request: IDBRequest<T>): Promise<T> {
	return new Promise((resolve, reject) => {
		request.onsuccess = () => resolve(request.result);
		request.onerror = () => reject(request.error ?? new Error('indexedDB request failed'));
	});
}

/** Saves a document under its board id and remembers it as the current board. */
export async function saveDocument(document: BoardDocument): Promise<void> {
	const db = await openDb();
	try {
		const tx = db.transaction(STORE, 'readwrite');
		await requestToPromise(tx.objectStore(STORE).put(document, document.board.id));
		await new Promise<void>((resolve, reject) => {
			tx.oncomplete = () => resolve();
			tx.onerror = () => reject(tx.error ?? new Error('indexedDB transaction failed'));
		});
	} finally {
		db.close();
	}
	try {
		localStorage.setItem(CURRENT_BOARD_KEY, document.board.id);
	} catch {
		// Blocked storage: the board still lives in IndexedDB for this session.
	}
}

/** Loads the current board, or `null` when none was saved. */
export async function loadCurrentDocument(): Promise<BoardDocument | null> {
	let id: string | null;
	try {
		id = localStorage.getItem(CURRENT_BOARD_KEY);
	} catch {
		id = null;
	}
	const db = await openDb();
	try {
		const store = db.transaction(STORE, 'readonly').objectStore(STORE);
		if (id) {
			const document = await requestToPromise(store.get(id));
			if (document) return document as BoardDocument;
		}
		// No remembered id (or it is gone): the first saved board, if any.
		const keys = await requestToPromise(store.getAllKeys());
		if (keys.length === 0) return null;
		return ((await requestToPromise(store.get(keys[0]))) as BoardDocument | undefined) ?? null;
	} finally {
		db.close();
	}
}

/** Serializes a document for export: pretty JSON, keys excluded by construction. */
export function exportText(document: BoardDocument): string {
	return JSON.stringify(document, null, 2) + '\n';
}

/** Parses an import; only the shape is checked here, the engine validates the content. */
export function parseImport(text: string): BoardDocument {
	const value: unknown = JSON.parse(text);
	if (
		typeof value !== 'object' ||
		value === null ||
		typeof (value as { schema_version?: unknown }).schema_version !== 'number' ||
		typeof (value as { board?: unknown }).board !== 'object'
	) {
		throw new Error('not a prostor board document');
	}
	return value as BoardDocument;
}

interface FilePickers {
	showSaveFilePicker?: (options: {
		suggestedName?: string;
		types?: { description: string; accept: Record<string, string[]> }[];
	}) => Promise<{
		createWritable(): Promise<{ write(data: string): Promise<void>; close(): Promise<void> }>;
	}>;
	showOpenFilePicker?: (options: {
		multiple?: boolean;
		types?: { description: string; accept: Record<string, string[]> }[];
	}) => Promise<{ getFile(): Promise<File> }[]>;
}

const JSON_TYPE = [{ description: 'prostor board', accept: { 'application/json': ['.json'] } }];

/** Writes the document to a file the user picks (or downloads it). */
export async function exportDocument(document: BoardDocument): Promise<void> {
	const text = exportText(document);
	const name = `${document.board.name.replace(/[^\w.-]+/g, '_') || 'board'}.prostor.json`;
	const pickers = window as unknown as FilePickers;
	if (pickers.showSaveFilePicker) {
		try {
			const handle = await pickers.showSaveFilePicker({ suggestedName: name, types: JSON_TYPE });
			const writable = await handle.createWritable();
			await writable.write(text);
			await writable.close();
			return;
		} catch (error) {
			if ((error as DOMException).name === 'AbortError') return;
			// Fall through to a download.
		}
	}
	const url = URL.createObjectURL(new Blob([text], { type: 'application/json' }));
	const anchor = globalThis.document.createElement('a');
	anchor.href = url;
	anchor.download = name;
	anchor.click();
	setTimeout(() => URL.revokeObjectURL(url), 1000);
}

/** Lets the user pick a file and returns its parsed document, or `null` if cancelled. */
export async function importDocument(): Promise<BoardDocument | null> {
	const pickers = window as unknown as FilePickers;
	if (pickers.showOpenFilePicker) {
		try {
			const [handle] = await pickers.showOpenFilePicker({ multiple: false, types: JSON_TYPE });
			return parseImport(await (await handle.getFile()).text());
		} catch (error) {
			if ((error as DOMException).name === 'AbortError') return null;
			throw error;
		}
	}
	return new Promise((resolve, reject) => {
		const input = globalThis.document.createElement('input');
		input.type = 'file';
		input.accept = 'application/json,.json';
		input.onchange = async () => {
			const file = input.files?.[0];
			if (!file) return resolve(null);
			try {
				resolve(parseImport(await file.text()));
			} catch (error) {
				reject(error);
			}
		};
		input.click();
	});
}
