# Handoff: Phase 0 milestones M1 to M3

Written 2026-10-14 when the local session stopped; updated by the cloud sessions that
built M1 and M2. Read `CLAUDE.md`, `docs/plan-phase0.md` (M1 section) and
`docs/review-backlog.md` first; ADRs 0003–0005 hold the design, `docs/protocol-ingest.md`
the wire reference.

## State

- `main`: foundations, spikes, review fixes, M1 (PR #1), the Docker cache-mount fix
  (PR #2), M2 (PR #3).
- `feat/m3-board-editor`: M3 (see "Done: M3" below).
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

## Done: browser half of M1 (first pass)

- `web/src/lib/render/`: `compositor.worker.ts` (frame loop, WebGL2 `renderer.ts`,
  encoders → wasm `FlvMuxer` → ingest client, status every 500 ms, preview through a
  transferred canvas), `pipeline.ts` (main-thread controller), `messages.ts`,
  `geometry.ts` (+ tests).
- `web/src/lib/adapters/`: `media.ts` (camera, Web Audio graph, processor pumps),
  `encoder.ts` (ADR 0005 config, fallback, queue cap, drop accounting, AAC priming
  constant), `ingest-ws.ts` (protocol v1 client, reconnect), `backpressure.ts` (ADR 0003
  §4 policy, + tests), `webcodecs.d.ts`.
- `/studio`: open camera → start/stop, room, secret (memory only), bitrate, status bar.
- `just e2e` (`web/scripts/e2e-record.mjs`): release server + headless Chrome with a fake
  camera, 10 s, ffprobe on the recording. Needs a Chrome with H.264 (`PROSTOR_CHROME`):
  Chrome for Testing has software H.264 on Linux and no AAC, so the session is video
  only there; the Playwright Chromium has no WebCodecs encoders at all.
- Backlog items 2 and 3 closed in `prostor-wasm` (BufferSource inputs, early window).

## Done: M2 (RTMP egress to platforms)

- `prostor_protocol::rtmp`: backlog 4, 6, 7, 8 fixed (terminal `Stopped`, query strings on
  the key, non-`Start` statuses as rejections, variant-name-only errors).
- `server/egress/rtmp.rs`: `RtmpEgress` (TCP, `tokio-rustls`, timeouts, private-address
  policy, reconnect with backoff, header resend, timestamp rebase, keyframe wait, caught
  panics, inbound cap). `Egress` reports `Delivery` and `Health`; the supervisor never
  blocks on an egress queue (drops instead). `EgressState` gained `reconnecting`,
  `reconnects`, `detail`.
- `prostor_model::signaling` + `server/signaling.rs` + `server/rooms.rs`: `/ws/room` with
  producer auth, `start_stream`/`stop_stream`; keys live in the room registry while the
  socket is open; the ingest session adds one `RtmpEgress` per declared destination.
  `Origin` check on both upgrades (`PROSTOR_ALLOWED_ORIGINS`), `PROSTOR_ALLOW_PRIVATE_DESTINATIONS`.
- Web: `adapters/signaling.ts`, `adapters/destinations.ts` (localStorage), destinations
  section on `/studio`, egress lines show reconnects and detail. `just e2e` also streams
  to an ffmpeg RTMP listener and probes its file.
- Tests: `tests/support/` has an in-process `rml_rtmp` server; `tests/rtmp_egress.rs`
  (publish, reconnect after a dropped socket, refused key, unreachable, private policy),
  `tests/signaling.rs` (auth, declarations, origins, a declaration driven through a real
  ingest session with one good and one dead destination).
- `docs/protocol-ingest.md` covers both sockets; plan M2 boxes ticked except the manual
  platform test; backlog items 4–8 closed.

## Done: M3 (document, board editor, scene editor)

- `prostor_model::command`: `Command` and `Event` (ts-rs); `prostor_engine::Engine`:
  `load` (schema + invariants), `apply` with validation, `snapshot`, `render_plan` of the
  live scene scaled from the 1920×1080 authoring space (backlog 12); wasm `Studio`
  (`new`, `load`, `dispatch`, `snapshot`, `live`, `render_plan`), browser-tested.
- Web: `board/store.svelte.ts` (runes store over `Studio`, debounced IndexedDB save,
  starter board, import/export), `adapters/storage.ts` (IndexedDB, File System Access
  API with download/upload fallback), `board/Board.svelte` (pan/zoom SVG, nodes, edges by
  dragging the handle, double-click or keys 1–9 for program), `board/SceneEditor.svelte`
  (16:9 editor with move/resize, layer list, fields, crop, z), `/studio` restructured;
  image layers render in the compositor.
- ADR 0006 written; plan M3 boxes ticked.

## Next

1. Run the M3 check by hand in Chrome: two scenes and an edge, reload, everything back;
   export, re-import; the program preview follows the live scene. Then M4 (transitions
   and the mixer) per the plan; it needs an `update_transition` command and partial layer
   patches (ADR 0006 consequences).
2. Manual platform test with the maintainer's keys (plan M2, last two boxes): Twitch and
   YouTube live within 10 s, unplug the network for 5 s, both recover, recording intact.
   Record reconnect behaviour per platform in `docs/spikes/0002` and accept ADR 0004.
   Watch for: the FP9 handshake, `flashVer`, the object-shaped `onMetaData` (ADR 0004
   risks), and whether platforms accept a stream that restarts at timestamp 0 after a
   reconnect.
3. The M1 check on macOS (hardware encoder + AAC): 60 s session, VLC playback, lag on
   loopback, A/V offset with a clap; tune `AAC_PRIMING_SAMPLES` from the measurement.
4. A fresh review of M1 to M3 (worker error paths, the browser reconnect path, the
   supervisor's drop policy under a wedged destination, the editor's command flow).

## Environment notes

- Local machine's `gh` and SSH key default to a work account; GitHub calls there use
  `GH_TOKEN="$(gh auth token --user kucherenko)"` over HTTPS. A cloud session needs its own
  GitHub connection to `kucherenko/prostor`.
- chromedriver 155 is under `~/.cache/prostor-browsers`; `just test-wasm` finds it.
- Cloud sessions: `just`, `wasm-pack` and `cargo-deny` come from `cargo install`; the web
  checks need `npm_config_engine_strict=false pnpm install` when the image's Node is a
  patch behind `engines.node`. `just test-wasm` needs a chromedriver matching the
  Playwright Chromium (`npx @puppeteer/browsers install chromedriver@<version> --path
  ~/.cache/prostor-browsers`, then `CHROMEDRIVER=...` and that Chromium on `PATH`).
  wasm-pack cannot download binaryen through the proxy: fetch the tarball with curl and put
  `wasm-opt` on `PATH`. `just e2e` needs Chrome for Testing
  (`npx @puppeteer/browsers install chrome@stable --path ~/.cache/prostor-browsers`,
  `PROSTOR_CHROME=<its chrome binary>`): software H.264, no AAC, and SwiftShader WebGL
  renders 1080p at about 12 fps there, so the frame-rate numbers are not meaningful on
  that box.
