# prostor — vision

**prostor** (Russian «простор»: open space, expanse) is an open-source live-production
studio. It combines a browser-based multi-guest streaming studio (think StreamYard) with an
infinite canvas (think Miro).

## What it is

- The producer works on an infinite **board**. On it they place **scenes**: 16:9
  compositions made of layers (guest video, screen share, media, text, images, overlays,
  browser sources).
- Scenes are connected with **transitions**, drawn as edges on the board. A show is a
  navigable graph of scenes, not a flat list.
- Guests join via a link and are assigned to one or more scenes. No accounts.
- The producer switches scenes live. The composed **program output** is streamed to
  YouTube, Twitch, TikTok and Telegram simultaneously, and can be recorded.

## How it is built

**Browser-first, thin server.** Everything that can run in the producer's browser runs
there: capture, guest media (WebRTC), compositing (OffscreenCanvas in a Worker), audio
mixing (Web Audio), encoding (WebCodecs) and FLV muxing (Rust compiled to WASM). The
server does only what a browser cannot: WebSocket signaling, RTMP(S) egress over TCP,
fan-out to several platforms, recording to disk, and later WHIP ingest and an SFU. The
Phase 0 server should be a few hundred lines of Rust around the shared core.

**Sans-IO Rust core.** The domain model, scene state machine, layout math, transition
timing, mixer decisions, FLV muxer and RTMP client live in `core/` and perform no I/O:
inputs are events, timestamps and byte slices; outputs are commands and byte buffers. The
same crates run natively on the server and as WASM in the browser. See ADR 0001 and 0002.

**Stack.** Rust (core + server, axum/tokio), Svelte 5 / SvelteKit + TypeScript (web app),
wasm-bindgen at the browser boundary. Naming: project, repo, Docker image and CLI binary
are `prostor`; crates are `prostor-*`; environment variables are `PROSTOR_*`.

## Phase 0 — walking skeleton

Board editor with scene nodes and transition edges; scene editor with positioned layers;
up to 4 guests over a WebRTC peer-to-peer mesh; a 1920×1080 @ 30 fps program renderer
(WebGL2, WebGPU behind a flag) with cut, crossfade and slide transitions; WebCodecs
H.264 + AAC (Opus fallback) encoder; FLV over binary WebSocket to the server; RTMP egress
to several destinations with reconnect and per-destination status; FLV recording; board
and destinations persisted in the browser; single-binary deploy via Docker. The manual
acceptance script lives in `docs/acceptance/phase0.md`.

## Later phases (context only)

- Phase 1: WHIP ingest for OBS and other encoders; server-side Opus→AAC if needed; MP4
  remux of recordings; platform OAuth; boards persisted on the server (SQLite).
- Phase 2: SFU for more than 4 guests; multi-user board editing (CRDT); WebTransport
  ingest; HLS preview.
- Phase 3: Tauri desktop shell running the server locally — no hosted infrastructure.

## Non-goals for Phase 0

Accounts, billing, SFU, plugins, themes, design system, database, Kubernetes.

## Principles

1. Sans-IO Rust core; thin, dumb adapters at the edges (browser APIs in TypeScript,
   tokio/sockets/files in the server). Policy lives in the core.
2. One protocol crate, two runtimes: FLV output produced in the browser is byte-identical
   to the native output for the same input, and CI proves it.
3. Rendering and encoding off the main thread; dropped frames and encoder queue depth are
   measured and shown from day one.
4. Document = data: versioned schema, stable ids, mutations as commands, so a CRDT can be
   added later without a rewrite.
5. Egress is an interface; no platform is a hard dependency.
6. Open source from commit one; no premature generality.
