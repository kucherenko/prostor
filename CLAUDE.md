# prostor — working notes for Claude Code

prostor is an open-source live-production studio: a browser-based multi-guest streaming
studio on an infinite canvas. Read `docs/vision.md` first. Decisions live in `docs/adr/`,
spike results in `docs/spikes/`, the current plan in `docs/plan-phase0.md`, the manual
acceptance test in `docs/acceptance/phase0.md`.

## Layout

- `core/` — sans-IO Rust crates, compiled to wasm32 and native: `prostor-model` (document
  types, ts-rs export), `prostor-engine` (scene state machine, layout, transitions),
  `prostor-protocol` (FLV muxer, RTMP client), `prostor-wasm` (wasm-bindgen facade).
- `server/prostor-server` — axum binary `prostor`: static files, signaling, ingest, egress,
  recording.
- `web/` — SvelteKit (Svelte 5 runes, TypeScript, Tailwind). Browser APIs live in
  `src/lib/adapters/`; the compositor worker in `src/lib/render/`.
- `spikes/` — throwaway feasibility code; results go to `docs/spikes/`.

## Commands

`just dev`, `just test`, `just test-wasm`, `just lint`, `just build`, `just docker`. See the
`justfile` for the rest.

## Rules

- Sans-IO core: no tokio, sockets, files, DOM or `web-sys` in `core/*`. Only
  `prostor-wasm` may depend on `wasm-bindgen`, `js-sys` and `web-sys`.
- Keep the app runnable end to end at every commit. Thin vertical slices over broad layers.
- Conventional commits. English for code, comments, docs and commits. No AI attribution
  lines in commits.
- Write an ADR (`docs/adr/NNNN-title.md`, 10–20 lines) for every decision that is expensive
  to reverse.
- Prefer feasibility spikes to reading docs for browser media APIs and RTMP: a short
  throwaway under `spikes/`, run it, record the result in `docs/spikes/`. Stop and ask
  after about two hours if a spike is not converging.
- Tests: thorough unit tests in `core/` (it is pure); `prostor-protocol` tests run both
  natively and with `wasm-pack test --headless --chrome`; a few integration tests for the
  server; do not chase coverage in UI code during Phase 0.
- Ask the maintainer before: choosing the license; choosing the AAC strategy; picking the
  WebRTC library; adding a dependency with more than 1 MB of WASM footprint; making WebGPU
  the default renderer; keeping ffmpeg on the hot path beyond the skeleton.
- Stream keys: kept in memory only, sent over WSS at stream start, never logged, never
  persisted, never part of the board document.
- Report in short status notes: what works, what is measured (fps, latency, CPU,
  bitrate), what is blocked.
- No accounts, billing, SFU, plugins, themes, design system, database or Kubernetes in
  Phase 0.
