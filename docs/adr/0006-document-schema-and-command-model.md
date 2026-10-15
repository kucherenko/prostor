# ADR 0006: Document schema v1 and the command model

- Status: accepted
- Date: 2026-10-15

## Context

The board is edited in the browser, persisted locally, exported as a file, and will later be
shared between a producer and guests and replicated through a CRDT (Phase 2). Scenes must
be renderable on any program size, and every edit must be validated in one place so the
compositor never sees a document it cannot draw.

## Decision

1. Schema v1 is `prostor_model::BoardDocument`: `schema_version` plus a `Board` of scenes,
   transitions, participants and destinations. Ids are opaque strings minted by the editor
   (`<kind>-<8 random hex>`), never array indices, so entries can be addressed, merged and
   replayed. Layer rectangles are authored in a fixed 1920×1080 space; the engine scales
   them to the actual program and keeps crops in source pixels.
2. Every mutation is a `Command` (scene add/move/rename/remove, layer add/update/remove,
   transition add/remove, set live, rename board) applied by `prostor_engine::Engine::apply`,
   which validates (unique ids, existing targets, finite rectangles with area that touch
   the program, transitions between two different scenes, bounded names) and answers with
   past-tense `Event`s, including what cascaded (a removed scene takes its transitions and
   the live selection with it). A refused command changes nothing. Commands carry the ids
   they create, so a log of commands replays to the same document.
3. The live selection is runtime state, not part of the document: a reloaded board starts
   with its first scene on program.
4. Stream keys are never in the document (ADR 0004); exports are safe to share.
5. Incompatible changes bump `schema_version` and ship a migration; `Engine::load` refuses
   other versions rather than guessing.

## Consequences

- The browser holds the document only through the wasm `Studio` facade; the UI dispatches
  commands and re-reads snapshots, so undo, a command log and CRDT replication can be added
  behind the same calls.
- `UpdateLayer` replaces a layer wholesale; partial patches and a transition update command
  are added when M4 needs them.
