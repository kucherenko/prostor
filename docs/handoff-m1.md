# Handoff: milestone M1 in progress

Written 2026-10-14 when the local session stopped. Read `CLAUDE.md`, `docs/plan-phase0.md`
(M1 section) and `docs/review-backlog.md` first; ADRs 0003–0005 hold the design.

## State

- `main`: foundations, spikes, review fixes; CI green (rust, wasm, web, docker, cargo-deny).
- `feat/m1-tracer-bullet` (pushed): `65cd1ba` shared types — `prostor_model::{render, ingest}`
  (`RenderPlan`, ingest protocol v1 messages with JSON-shape tests, binary frame layout in
  the module doc), `prostor_engine::layout::render_plan`, `prostor_protocol::ingest::
  {SEQ_LEN, encode_frame, parse_frame}`, `flv::write_metadata_tag`, wasm `render_plan`,
  regenerated TS types; `f447521` `prostor_server::shutdown::Shutdown` (CancellationToken +
  TaskTracker, not wired yet); `dfc0a03` review backlog.
- Maintainer decisions: Apache-2.0; pipeline-first order; "proceed with the implementation".
  Still open: AAC strategy (ADR 0005, option A recommended), a Twitch test key, a
  `spikes/webcodecs` run on any other producer OS.

## Next: server half of M1 (nothing under `server/prostor-server/src/egress/` exists)

1. `egress/mod.rs`: `#[async_trait] trait Egress { start(&StreamHeaders); push(seq, &TagBuf);
   stop() }`, `StreamHeaders { info: StreamInfo, metadata: TagBuf, video_sequence_header,
   audio_sequence_header }`, `EgressError`.
2. `egress/file.rs`: `FileEgress` — FLV file header, metadata, cached sequence headers, tags;
   `BufWriter<tokio::fs::File>`, flush + sync on stop; `<room>-<UTC yyyymmdd-hhmmss>.flv` in
   `PROSTOR_RECORDINGS_DIR` (created at startup); returns the file name only.
3. `egress/supervisor.rs`: header cache from the first AVC/AAC sequence headers (media before
   the video sequence header → `Protocol` error); one task + bounded mpsc (192) per egress;
   on a full queue drop non-key video until the next keyframe, never audio or script, count
   drops; `watch` channel per egress with `EgressState`; `acked_seq()` = min `last_seq` over
   live egresses; `stop(timeout)`.
4. `ingest.rs` for `/ws/ingest/{room}` (replace the stub in `ws.rs`): first message a text
   `Hello` within 5 s; `auth` verified with `Secret::verify` when `PROSTOR_SECRET` is set →
   else `Error{Unauthorized}` + close 4401; non-Hello first → `Error{BadMessage}` + 4400;
   reply `Started{room, recording}`; binary frames → `parse_frame` → supervisor; `Ack{seq,
   server_ms}` every 100 ms when changed; `EgressStatus` every 1 s; `EndOfStream` → stop →
   `Ended` → close 1000; protocol errors → `Error` + 4400, never echoing client bytes; keep
   size limits, idle timeout (fix backlog item 1), ping, socket semaphore; never log `auth`.
5. Shutdown wiring: handlers select on `shutdown.cancelled()` → Close 1001, stop supervisor;
   `main` uses `with_graceful_shutdown` then `shutdown.wait(10 s)`; second signal exits.
6. `/healthz`: add `recordings_dir_writable`, `ingest_sessions`.
7. Tests `tests/ingest.rs` (real listener + tokio-tungstenite + tempfile): full session with
   the media tags of `core/prostor-protocol/tests/fixtures/testsrc-720p-1s.flv` → recording
   parses back byte-for-byte (+ `ffprobe` h264/aac if on PATH); wrong secret 4401; non-Hello
   4400; media before sequence header; malformed frame; cancellation → 1001 + finalized
   file; slow egress → audio never dropped, video dropped until keyframe, drops reported.
8. `docs/protocol-ingest.md`: one-page wire reference (auth, message table, frame layout,
   acks, close codes, example session). Tick M1 server items in the plan.

Gate before each commit: `just lint && just test && just smoke && cargo deny check`.

## Then: browser half of M1

`web/src/lib/render/compositor.worker.ts` (OffscreenCanvas + WebGL2 1920×1080 @ 30 fps
from a `RenderPlan`, VideoFrames with one clock, preview via a transferred canvas),
`adapters/media.ts` (getUserMedia → `MediaStreamTrackProcessor` on the main thread →
per-frame transfer; Web Audio graph → destination track → AudioData to the Worker),
`adapters/encoder.ts` (ADR 0005 config, hardware → software fallback, keyframe every 60,
queue accounting; watch backlog items 2 and 3 for `FlvMuxer` inputs and origins),
`adapters/ingest-ws.ts` (ADR 0003 policy: seq prefix, acks, lag, drop-until-keyframe,
bitrate step-down, end_of_stream), `/studio` page with Start/Stop, secret, status bar; an
e2e script with Chrome's fake camera that records 10 s and checks the file with ffprobe.
Then a fresh review and a PR to `main`.

## Environment notes

- Local machine's `gh` and SSH key default to a work account; GitHub calls there use
  `GH_TOKEN="$(gh auth token --user kucherenko)"` over HTTPS. A cloud session needs its own
  GitHub connection to `kucherenko/prostor`.
- chromedriver 155 is under `~/.cache/prostor-browsers`; `just test-wasm` finds it.
