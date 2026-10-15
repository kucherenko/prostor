# render

The compositor Worker and its main-thread controller.

- `compositor.worker.ts`: the Worker entry. Runs the frame loop (a drift-corrected timer
  at the program's rate), draws the `RenderPlan` with `renderer.ts`, turns each frame
  into a `VideoFrame` for `adapters/encoder`, muxes the chunks with the wasm `FlvMuxer`
  and sends the tags through `adapters/ingest-ws`. One clock: the Worker's
  `performance.now()` at session start is the muxer origin; camera frames are drawn as
  they arrive, audio timestamps are mapped onto the same clock minus the AAC priming.
- `renderer.ts`: WebGL2 on an `OffscreenCanvas`; textured quads for the camera (latest
  `VideoFrame`), text (rasterised to a texture once per text) and colours. Never a
  2D-canvas frame source: spike 0001 showed it starves the hardware encoder.
- `geometry.ts`: program pixels → clip space, and the sampled source region (crop or a
  centred cover fit).
- `audio-hold.ts`: the bounded queue for audio that arrives before the video sequence
  header; an evicted chunk hands its decoder configuration on, so the audio sequence
  header is never lost.
- `pipeline.ts` (main thread): starts the Worker, transfers the preview canvas, opens
  capture with `adapters/media` and forwards frames; exposes `start`/`stop`.
- `messages.ts`: the page ↔ Worker protocol and the `PipelineStatus` the status bar shows.

WebGPU stays behind a flag until ADR 0007 (M4). The audio mixer is a single gain node
until M5 adds guests.
