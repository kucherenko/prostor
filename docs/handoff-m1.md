# Handoff: milestone M1 in progress

Written 2026-10-14 when the local session stopped; updated by the cloud session that
built the server half. Read `CLAUDE.md`, `docs/plan-phase0.md` (M1 section) and
`docs/review-backlog.md` first; ADRs 0003–0005 hold the design, `docs/protocol-ingest.md`
the wire reference.

## State

- `main`: foundations, spikes, review fixes; CI green (rust, wasm, web, docker, cargo-deny).
- `feat/m1-tracer-bullet`: `65cd1ba` shared types (`prostor_model::{render, ingest}`,
  `prostor_engine::layout::render_plan`, `prostor_protocol::ingest`, wasm `render_plan`,
  TS types); `f447521` `Shutdown` primitives; `dfc0a03` review backlog; then the server
  half of M1 (see below).
- Maintainer decisions: Apache-2.0; pipeline-first order; "proceed with the implementation".
  Still open: AAC strategy (ADR 0005, option A recommended), a Twitch test key, a
  `spikes/webcodecs` run on any other producer OS.

## Done: server half of M1

- `egress/mod.rs`: `trait Egress { id, kind, start(&StreamHeaders), push(seq, &TagBuf),
  stop }`, `StreamHeaders`, `EgressError`, `metadata_tag(&StreamInfo)`.
- `egress/file.rs`: `FileEgress::create(dir, room, now)` opens
  `<room>-<UTC yyyymmdd-hhmmss>[-n].flv` up front (so a failure precedes `started`),
  `start` writes header + metadata + cached sequence headers, `stop` flushes and syncs.
- `egress/supervisor.rs`: order validation (video sequence header first, audio header
  before the first audio frame, no script tags, no audio in video-only streams,
  increasing `seq`), header cache, lazy start on the first media frame, one task +
  `mpsc(192)` per egress, drop-until-keyframe for deltas only (audio/key/headers wait),
  per-egress `watch<EgressState>`, `acked_seq()` = min `last_seq` over live egresses,
  `stop(timeout)` aborts stragglers, `Drop` still finalizes a started egress.
- `ingest.rs`: the `/ws/ingest/{room}` session exactly as `docs/protocol-ingest.md`
  describes (hello timeout, `Secret::verify`, `StreamInfo` validation, acks every 100 ms
  on change, status every 1 s, `end_of_stream` → drain → `ended` → 1000, errors → `error`
  + 4400/4401/1011, never echoing client bytes, never logging `auth`).
- `ws.rs`: `Peer` with one idle deadline per socket (pushed by received frames only, pongs
  included), pings, bounded close-handshake drain, `Timeouts` for tests; `room` stub also
  honours shutdown. `app.rs`: `router(config, shutdown)`, `/healthz` adds
  `recordings_dir_writable` and `ingest_sessions`. `main.rs`: creates the recordings
  directory, warns when unauthenticated on a non-loopback bind, graceful shutdown with
  `DRAIN_TIMEOUT`, second signal exits.
- Tests: `tests/ingest.rs` (fixture replay → recording parses back tag for tag, `ffprobe`
  h264/aac when on PATH, 4401, 4400 variants, media before the header, malformed frame,
  shutdown → 1001 + finalized file, vanished peer, `/healthz` session count),
  `tests/ws.rs` (close handshake, idle vs pong-answering peers, `MAX_SOCKETS`, shutdown),
  supervisor unit tests (slow egress: audio never dropped, deltas dropped until the next
  keyframe, drops reported, failed egress skipped, hung egress aborted).
- Backlog items 1, 9, 10, 11 closed; plan boxes ticked.

## Next: browser half of M1

`web/src/lib/render/compositor.worker.ts` (OffscreenCanvas + WebGL2 1920×1080 @ 30 fps
from a `RenderPlan`, VideoFrames with one clock, preview via a transferred canvas),
`adapters/media.ts` (getUserMedia → `MediaStreamTrackProcessor` on the main thread →
per-frame transfer; Web Audio graph → destination track → AudioData to the Worker),
`adapters/encoder.ts` (ADR 0005 config, hardware → software fallback, keyframe every 60,
queue accounting; watch backlog items 2 and 3 for `FlvMuxer` inputs and origins),
`adapters/ingest-ws.ts` (ADR 0003 policy: seq prefix, acks, lag, drop-until-keyframe,
bitrate step-down, end_of_stream; the server's exact behaviour is in
`docs/protocol-ingest.md` and `server/prostor-server/tests/ingest.rs`), `/studio` page with
Start/Stop, secret, status bar; an e2e script with Chrome's fake camera that records 10 s
and checks the file with ffprobe. Then a fresh review and a PR to `main`.

Server follow-ups worth a look during the browser half: the ack cadence (100 ms, ADR 0003
says tune it once the protocol exists) and whether a keyframe should also be droppable when
an egress queue stays full (today only deltas are dropped, so a wedged egress stalls the
ingest reader on the next keyframe or audio tag until `stop` aborts it).

Gate before each commit: `just lint && just test && just smoke && cargo deny check`.

## Environment notes

- Local machine's `gh` and SSH key default to a work account; GitHub calls there use
  `GH_TOKEN="$(gh auth token --user kucherenko)"` over HTTPS. A cloud session needs its own
  GitHub connection to `kucherenko/prostor`.
- chromedriver 155 is under `~/.cache/prostor-browsers`; `just test-wasm` finds it.
- Cloud sessions: `just`, `wasm-pack` and `cargo-deny` come from `cargo install`; the web
  checks need `npm_config_engine_strict=false pnpm install` when the image's Node is a
  patch behind `engines.node`; `just test-wasm` works with
  `CHROMEDRIVER=/opt/node22/bin/chromedriver` and the Playwright Chromium on `PATH`.
