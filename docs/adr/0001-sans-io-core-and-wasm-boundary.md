# ADR 0001: Sans-IO Rust core and the WASM boundary

- Status: accepted
- Date: 2026-10-14

## Context

The same logic must run in the producer's browser and on the server: the board document,
the scene state machine, layout and transition math, FLV muxing and the RTMP client.
Browser APIs and sockets differ per runtime. Writing the logic twice (TypeScript and Rust)
would drift; coupling it to one runtime's I/O would make it untestable and unportable.

## Decision

1. Crates under `core/` are sans-IO. Inputs are events, timestamps and byte slices; outputs
   are commands and byte buffers. No tokio, sockets, filesystem, DOM or `web-sys`. Time is
   always passed in, never read.
2. Every `core/` crate compiles for `wasm32-unknown-unknown` and native targets. CI runs
   `cargo test` natively and `wasm-pack test --headless --chrome` for `prostor-protocol`
   and `prostor-wasm`. A fixture test asserts byte-identical FLV output in both runtimes.
3. `prostor-wasm` is the only crate that depends on `wasm-bindgen`, `js-sys` or `web-sys`.
   It is a facade: it owns the engine instance, accepts commands and returns plain data
   (serde via `serde-wasm-bindgen`) or byte buffers (`Uint8Array`). Native-only
   dependencies (for example an RTMP session library) are feature-gated out of wasm.
   The RTMP module is the one documented exception to "time is always passed in": the
   library underneath reads the clock and the RNG itself (ADR 0004).
4. TypeScript types are generated from `prostor-model` with `ts-rs` into
   `web/src/lib/generated/` and committed; CI fails when they are stale. The model crate
   stays free of wasm dependencies.
5. The compositor and encoder in the browser execute per-frame layout and transition
   state computed by the Rust engine; the TypeScript adapters execute, they do not decide.

## Consequences

- Policy logic gets cheap, exhaustive, deterministic unit tests and replay.
- One implementation of FLV and RTMP is shared by browser and server.
- A thin TypeScript layer still wraps every browser API; the WASM↔JS call overhead at
  30 fps must be measured (spike c) and kept to a few calls per frame.
- Generated TS types add a `just gen-types` step to the workflow.
