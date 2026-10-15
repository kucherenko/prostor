// The board editor's state: a wasm `Studio` (the engine) behind Svelte runes. Every edit
// is a `Command` dispatched to the engine; the store then re-reads the snapshot (boards
// are small), persists it, and recomputes the live scene's render plan for whoever
// listens (the compositor).

import type { BoardDocument } from '#lib/generated/BoardDocument';
import type { Command } from '#lib/generated/Command';
import type { Event } from '#lib/generated/Event';
import type { Layer } from '#lib/generated/Layer';
import type { LayerKind } from '#lib/generated/LayerKind';
import type { RenderPlan } from '#lib/generated/RenderPlan';
import type { Scene } from '#lib/generated/Scene';
import type { Transition } from '#lib/generated/Transition';
import { loadCurrentDocument, saveDocument } from '#lib/adapters/storage';
import { loadWasm } from '#lib/wasm';
import { Studio } from '#lib/wasm/pkg/prostor_wasm.js';

export const PROGRAM_WIDTH = 1920;
export const PROGRAM_HEIGHT = 1080;
const SAVE_DELAY_MS = 300;

export function newId(prefix: string): string {
	const random =
		typeof crypto !== 'undefined' && 'randomUUID' in crypto
			? crypto.randomUUID().slice(0, 8)
			: Math.random().toString(36).slice(2, 10);
	return `${prefix}-${random}`;
}

/** A layer of `kind` with a sensible default placement. */
export function defaultLayer(kind: LayerKind, name: string, z: number): Layer {
	const full = { x: 0, y: 0, w: PROGRAM_WIDTH, h: PROGRAM_HEIGHT };
	const rect =
		kind.type === 'text'
			? { x: 96, y: 880, w: 1200, h: 120 }
			: kind.type === 'color'
				? { x: 0, y: 0, w: 480, h: 270 }
				: full;
	return { id: newId('layer'), name, kind, rect, z, crop: null };
}

/** The board every fresh install starts with: one scene, camera plus a title. */
export function starterCommands(sceneId: string): Command[] {
	return [
		{ type: 'add_scene', id: sceneId, name: 'Scene 1', position: { x: 80, y: 80 } },
		{ type: 'add_layer', scene: sceneId, layer: defaultLayer({ type: 'camera' }, 'Camera', 0) },
		{
			type: 'add_layer',
			scene: sceneId,
			layer: defaultLayer({ type: 'text', text: 'prostor' }, 'Title', 1)
		},
		{ type: 'set_live', scene: sceneId }
	];
}

export class BoardStore {
	doc = $state<BoardDocument>() as BoardDocument;
	live = $state<string | null>(null);
	selectedScene = $state<string | null>(null);
	selectedTransition = $state<string | null>(null);
	selectedLayer = $state<string | null>(null);
	/** Set when persistence failed; the editor keeps working in memory. */
	storageError = $state<string | null>(null);
	/** The last refused command, for the UI. */
	lastError = $state<string | null>(null);

	private studio: Studio;
	private saveTimer: ReturnType<typeof setTimeout> | null = null;

	private constructor(studio: Studio) {
		this.studio = studio;
		this.doc = studio.snapshot() as BoardDocument;
		this.live = studio.live() ?? null;
	}

	/** Loads the saved board, or creates the starter board. */
	static async open(): Promise<BoardStore> {
		await loadWasm();
		let saved: BoardDocument | null = null;
		let storageError: string | null = null;
		try {
			saved = await loadCurrentDocument();
		} catch (error) {
			storageError = `cannot read the saved board: ${describe(error)}`;
		}
		let store: BoardStore;
		if (saved) {
			try {
				store = new BoardStore(Studio.load(saved));
			} catch (error) {
				storageError = `saved board rejected (${describe(error)}); starting fresh`;
				store = BoardStore.starter();
			}
		} else {
			store = BoardStore.starter();
		}
		store.storageError = storageError;
		// A reloaded board has no live scene yet: put the first one on program.
		if (store.live === null && store.doc.board.scenes.length > 0) {
			store.dispatch({ type: 'set_live', scene: store.doc.board.scenes[0].id });
		}
		store.selectedScene = store.live;
		return store;
	}

	private static starter(): BoardStore {
		const store = new BoardStore(new Studio(newId('board'), 'My show'));
		for (const command of starterCommands(newId('scene'))) store.dispatch(command);
		return store;
	}

	get scenes(): Scene[] {
		return this.doc.board.scenes;
	}

	get transitions(): Transition[] {
		return this.doc.board.transitions;
	}

	scene(id: string | null): Scene | undefined {
		return id === null ? undefined : this.scenes.find((s) => s.id === id);
	}

	/** Applies a command; returns the events, or `null` when the engine refused it. */
	dispatch(command: Command): Event[] | null {
		let events: Event[];
		try {
			events = this.studio.dispatch(command) as Event[];
		} catch (error) {
			this.lastError = describe(error);
			return null;
		}
		this.lastError = null;
		this.doc = this.studio.snapshot() as BoardDocument;
		this.live = this.studio.live() ?? null;
		if (this.selectedScene !== null && !this.scene(this.selectedScene)) {
			this.selectedScene = null;
			this.selectedLayer = null;
		}
		if (
			this.selectedTransition !== null &&
			!this.transitions.some((t) => t.id === this.selectedTransition)
		) {
			this.selectedTransition = null;
		}
		this.scheduleSave();
		return events;
	}

	/** What the compositor should draw now. */
	renderPlan(width = PROGRAM_WIDTH, height = PROGRAM_HEIGHT): RenderPlan {
		return this.studio.render_plan(performance.now(), width, height) as RenderPlan;
	}

	/** Replaces the board with an imported document (validated by the engine). */
	replace(document: BoardDocument): void {
		const studio = Studio.load(document);
		this.studio.free();
		this.studio = studio;
		this.doc = studio.snapshot() as BoardDocument;
		this.live = null;
		this.selectedScene = null;
		this.selectedLayer = null;
		this.selectedTransition = null;
		this.lastError = null;
		if (this.scenes.length > 0) this.dispatch({ type: 'set_live', scene: this.scenes[0].id });
		else this.scheduleSave();
	}

	private scheduleSave() {
		if (this.saveTimer) clearTimeout(this.saveTimer);
		this.saveTimer = setTimeout(() => {
			this.saveTimer = null;
			saveDocument(this.doc).then(
				() => (this.storageError = null),
				(error) => (this.storageError = `cannot save the board: ${describe(error)}`)
			);
		}, SAVE_DELAY_MS);
	}

	/** Flushes a pending save (before leaving the page). */
	async flush(): Promise<void> {
		if (this.saveTimer) {
			clearTimeout(this.saveTimer);
			this.saveTimer = null;
			await saveDocument(this.doc).catch(() => undefined);
		}
	}
}

function describe(error: unknown): string {
	return error instanceof Error ? error.message : String(error);
}
