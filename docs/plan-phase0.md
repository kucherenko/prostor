# Phase 0 plan — walking skeleton

Status: **proposed, waiting for the maintainer's review** (first session, 2026-10-14).

Each milestone is a thin vertical slice that leaves the app runnable end to end and ends with
a check a human can run in minutes. Boxes are ticked in this file as work lands. Spike
reports: `docs/spikes/0001` (WebCodecs), `0002` (FLV + RTMP), `0003` (WASM identity),
`0004` (WebSocket backpressure). ADRs 0003–0005 are *proposed* from the spikes and are part
of this review.

## Decisions needed from the maintainer

- [x] **License**: Apache-2.0 (decided 2026-10-14; `LICENSE` and `license` fields in place).
- [x] **Scene membership**: spatial — a scene's frame streams what lies inside it (decided
      2026-10-15, ADR 0007).
- [ ] **AAC strategy** (ADR 0005, option A or B). Recommendation: A — WebCodecs AAC-LC on
      macOS/Windows for Phase 0, server-side Opus → AAC (ffmpeg sidecar) in Phase 1.
- [ ] **Milestone order**: pipeline first (M1–M2 below, recommended: the remaining risk is
      in the media path, and a platform test needs a working pipeline) or board first (swap
      M3 before M1; the board is low-risk UI work).
- [ ] **Twitch test stream key** to finish spike (b) on a real platform and accept ADR 0004.
      Run: `PROSTOR_SPIKE_RTMP_URL='rtmp://live.twitch.tv/app/<key>' cargo run -p rtmp-publish -- --input spikes/rtmp-publish/fixtures/testsrc-720p.flv --realtime --loop 10`
      (the key is read from the environment and redacted in all output).
- [ ] **Producer OS**: spike (a) ran on this macOS machine. Open `spikes/webcodecs` on any
      other OS you produce from (see its README) before accepting ADR 0005.
- [ ] WebRTC server library (`str0m` vs `webrtc-rs`): Phase 1 only; nothing in Phase 0
      needs server-side WebRTC.

## What the spikes established

| Spike | Result | Key numbers (M1 Pro, Chrome 155, machine under load 7–19) |
| --- | --- | --- |
| a WebCodecs | Browser-first encoding works with margin, in a Worker fed by WebGL2 frames | HW H.264 1080p30: 11.5 ms mean latency, 15–23 % of a core, 160 fps ceiling; SW: ≈ 1 core; AAC-LC available (`description` `11 90`); Main 4.0 is the minimum level; two Chrome stalls documented (2D-canvas frames in a Worker; paced HW encode on the main thread) |
| b FLV + RTMP | Muxer matches ffmpeg byte for byte; `rml_rtmp` publishes correctly to an RTMP server | 82/82 tags identical; handshake → publishing 1.7 ms; 466/466 tags received; Twitch/YouTube pending a key |
| c WASM identity | Same output hash natively and in Chrome; boundary cost is noise | 0.41 µs per `write_video` (release); JS→WASM 9.7 µs per video tag; wasm package 44 KB |
| d WS ingest | Throughput is a non-issue; queue *visibility* is the issue; server acks fix it | 6 Mbit/s at 1–4 ms; server ceiling 4.2 Gbit/s per connection at 29 % of a core; 2.3 MB hides below `bufferedAmount`; ack-driven drop policy holds p95 0.6 s vs 4–6 s; CDP throttling does not apply to WebSockets; batching hurts |

## Milestones

### M0 — Foundations (done in the first session)

- [x] `docs/vision.md`; ADR 0001 (sans-IO core, WASM boundary), 0002 (browser-first).
- [x] Workspace (`core/*`, `server/*`, `spikes/*`), SvelteKit 3 app, Svelte → WASM → `Scene`
      round trip with ts-rs types, axum server serving the SPA, CI workflow, `justfile`,
      multi-stage `Dockerfile` (111 MB image), `docker-compose.yml`, smoke test.
- [x] `prostor-protocol`: FLV muxer/parser + AMF0 + `rml_rtmp` wrapper, tests in both
      runtimes; `FlvMuxer` exported from `prostor-wasm`.
- [x] Spikes a–d; ADRs 0003–0005 proposed.
- [x] Apply the fresh-context review findings on `prostor-protocol` and the server
      (bounded AMF0 decoder, limited and sticky FLV demuxer, failure-atomic RTMP session
      with typed rejections and scrubbed server text, shared `Secret` type, validated
      room ids, WebSocket size limits and idle timeouts, loopback default bind, SPA
      fallback only for app routes, query strings kept out of logs, `cargo deny`).
- [x] LICENSE + `license =` fields; first commit.
- [x] Push to https://github.com/kucherenko/prostor; CI green on the second run (the
      first failed only on the pnpm setup step reading the wrong `package.json`).
- [ ] Deferred review items (see also M1/M2): a `cargo-fuzz` job for `parse_tag`,
      `Demuxer`, `amf0::read_value` and `Header::parse`.

### M1 — Tracer bullet: camera to a recorded FLV

Goal: a fixed scene (camera + text) is rendered, encoded, muxed in WASM, sent over the
ingest socket and written to disk by the server as a playable FLV.

- [x] `render/compositor.worker.ts`: OffscreenCanvas + WebGL2 at 1920×1080 @ 30 fps; executes
      the `RenderPlan` of the sample scene (camera + title) for now; produces `VideoFrame`s
      with timestamps from one clock; preview through a transferred canvas.
- [x] `adapters/media`: camera via `getUserMedia` → `MediaStreamTrackProcessor` on the main
      thread → per-frame transfer to the Worker; Web Audio graph → destination track →
      `AudioData` to the Worker's `AudioEncoder`.
- [x] `adapters/encoder` (in the Worker): ADR 0005 config, hardware → software fallback,
      keyframe every 60 frames, queue-depth and silent-drop accounting. AAC priming is
      compensated with the usual 2112 samples (44 ms); the clap measurement that tunes it
      is still to do.
- [x] `adapters/ingest-ws` (in the Worker): ADR 0003 protocol v1 — `u32` seq + FLV tag per
      message, acks, lag, drop-until-keyframe, bitrate step-down, `end_of_stream`;
      reconnect with cached sequence headers (policy unit-tested in `backpressure.ts`).
- [x] Server: ingest handler parses tags with `prostor-protocol`, acks every 100 ms,
      `trait Egress` + `FileEgress` (`PROSTOR_RECORDINGS_DIR/<room>-<timestamp>.flv`,
      FLV header + metadata + sequence headers written by the server); egress supervisor
      with bounded queues and drop-until-keyframe; `docs/protocol-ingest.md`; integration
      tests replay the FLV fixture through a real socket and parse the recording back.
- [x] Status bar: fps, dropped frames, encoder queue, active encoder, lag, `bufferedAmount`,
      bitrate, plus the server's per-egress state. `just e2e` streams 10 s from Chrome's
      fake camera and checks the recording with ffprobe.
- [x] Graceful shutdown that covers WebSockets (review #7): a `CancellationToken` +
      `TaskTracker`; handlers send Close 1001 and finalize (flush the recording); a
      deadline after `serve` returns; exit on the second signal.
- [x] Until M2 adds producer auth, the ingest socket must not be reachable
      unauthenticated: `hello.auth` is checked against `PROSTOR_SECRET` when set; without
      a secret the server warns unless bound to loopback (the default).
- Check: 60 s session → `ffprobe` shows H.264 Main + AAC-LC, the file plays in VLC, lag
  stays < 100 ms on loopback, A/V offset measured with a clap.

### M2 — RTMP egress to platforms

Goal: the same stream reaches Twitch and YouTube at once, survives a disconnect, never leaks
a key.

- [x] Signaling socket `/ws/room/<id>`: producer auth with `PROSTOR_SECRET`, `start_stream`
      carrying destinations + keys (memory only, redacted `Debug`), `stop_stream`; typed
      messages in `prostor-model` (ts-rs). Per-destination status reaches the producer in
      the ingest socket's `egress_status`, which the studio already shows.
- [x] `RtmpEgress` (ADR 0004): tokio TCP + `tokio-rustls` for `rtmps://`, per-destination
      task with a bounded ~2 s queue, drop-until-keyframe decided with `Tag::class()`,
      reconnect with backoff resending cached headers and rebasing timestamps,
      `SetPeerBandwidth` tolerated. Audio is dropped only while a queue is full or the
      connection is down (the supervisor never blocks the ingest socket on one egress).
- [x] Connect-time safety (review #16): resolve the destination host and refuse
      loopback, link-local and private ranges unless `PROSTOR_ALLOW_PRIVATE_DESTINATIONS`;
      connect, publish and write timeouts.
- [x] Producer auth + `Origin` check on both WebSocket upgrades (review #5): same-origin by
      default, `PROSTOR_ALLOWED_ORIGINS` otherwise; constant-time `Secret::verify`.
- [x] Egress supervisor per session: header cache, fan-out to all egresses incl.
      `FileEgress`, per-destination status (starting / live / degraded / reconnecting /
      failed / stopped, bytes, drops, reconnects, detail).
- [x] Destinations UI: name, URL, key (masked, stored in the browser, absent from exports).
- [x] Integration test: in-process `rml_rtmp` server session accepts a publish and receives
      the tags; a reconnect test with a dropped socket; a declaration driven through a real
      ingest session; `just e2e` streams to an ffmpeg RTMP listener too.
- [ ] Manual platform test with the maintainer's keys; record reconnect behaviour per
      platform (YouTube vs Twitch) in `docs/spikes/0002`; accept ADR 0004.
- [ ] `FfmpegEgress` only if the pure-Rust path stalls (brief §4, same trait).
- Check: both platforms live within 10 s; unplug the network for 5 s; both recover; the
  recording is intact.

### M3 — Document, board editor, scene editor

Goal: scenes come from the board, not from code, and survive a reload.

- [x] `prostor-model`: freeze schema v1 (string ids, `schema_version`); `Command` enum
      (scene add/move/remove, layer add/update/remove, transition add/remove, set live) and
      `Event` enum; ts-rs types.
- [x] `prostor-engine`: `Engine::apply(cmd) -> Result<Vec<Event>>` with validation (dangling
      ids, duplicates, program bounds, names); `RenderPlan` from the live scene, scaled from
      the 1920×1080 authoring space (backlog 12); unit tests.
- [x] `prostor-wasm`: `Studio` facade (`load`, `dispatch`, `snapshot`, `render_plan(now)`).
- [x] `/studio` board: pan/zoom canvas, scene nodes (create, move, delete), transition edges,
      live scene by double-click or keys 1–9; program preview is the compositor's output.
- [x] Scene editor: layers (camera, screen share, image, text, color) with position, size,
      z-order, crop in a 16:9 editor. Image layers render in the compositor; screen share
      and guests draw a placeholder until M5.
- [x] `adapters/storage`: IndexedDB persistence; import/export via the File System Access
      API (download/upload fallback).
- [x] ADR 0006: document schema and command model (ids, versioning, CRDT readiness).
- Check: build two scenes and an edge, reload, everything is back; export, re-import; the
  compositor renders whatever the board says.
- Superseded in part by M3.5: per-scene layers and the scene editor give way to canvas
  elements and frames (ADR 0007).

### M3.5 — Lobby, canvas and scene frames (ADR 0007)

Goal: the producer enters through a lobby, builds the show on one canvas, and streams what
the live frame shows.

- [ ] Lobby: `/studio` opens on camera and microphone pickers (`enumerateDevices`) with a
      live preview, a display name, the room and the producer secret → Join. Device ids and
      the name stay in localStorage, never in the board. One component, reused by the M5
      guest page. `just e2e` follows lobby → Join → Start.
- [ ] `prostor-model` schema v2: board-level `elements`, `Scene.frame`; frozen `v1` types
      and the v1 → v2 migration for saved boards and imports; element commands and
      `set_scene_frame`; ts-rs types.
- [ ] `prostor-engine`: validation (16:9 frames, ids unique on the board); `render_plan`
      from the live frame (overlapping elements, projected, not clamped); unit tests with
      migration fixtures; wasm tests.
- [ ] Compositor: destination rectangles that overflow the program are cut at its edge.
- [ ] Canvas UI: a full-viewport canvas with a toolbar (select, camera, screen share, image,
      text, sticker, frame); elements drawn live under pan/zoom; frames on top with their
      names, the live one marked; select, drag, resize; double-click or 1–9 to go live. The
      scene editor is removed.
- [ ] Screen share through `getDisplayMedia`; per-source textures in the compositor (camera,
      screen); a bundled sticker set inserted as image elements.
- Check: pick a non-default camera and a name in the lobby, join; put the camera, a sticker
  and a screen share on the canvas; frame them wide, then frame the camera alone as a
  close-up; go live on each with 1 and 2. The program and the recording show exactly what
  each frame shows, cut at its edge. Reload: everything is back; a v1 export imports.

### M4 — Transitions and audio mixer

- [ ] `prostor-engine`: transition state machine (cut, crossfade, slide; duration; progress
      from timestamps); `RenderPlan` carries both scenes and the progress during a transition.
- [ ] Compositor implements the three transitions in WebGL2; mixer fades per-source gains
      with the transition.
- [ ] ADR 0008: renderer (WebGL2 default, WebGPU behind a flag).
- Check: scene 1 → scene 2 crossfade visible on both platforms and in the recording.

### M5 — Guests over a WebRTC mesh

- [ ] Room tokens; `/join/[room]?t=` guest page (the lobby's device picker, mute, leave);
      one `RTCPeerConnection` per guest to the producer; public STUN, no TURN (documented).
- [ ] Producer: up to 4 peer connections; received video tracks read on the main thread and
      transferred to the Worker; guest audio into the mixer; `Guest` element kind (a
      guest is in a scene when their element lies in its frame); presence in the UI.
- [ ] Signaling: offer/answer/ICE relay, presence, guest-to-scene assignment events.
- Check: steps 5, 6 and 9 of `docs/acceptance/phase0.md`.

### M6 — Deploy, measure, pass the acceptance script

- [ ] Deploy with `docker compose up` on a VPS behind a reverse proxy for WSS; README.
- [ ] Validate the ADR 0003 thresholds on the real path (remote server, `dummynet`).
- [ ] Run `docs/acceptance/phase0.md` end to end; record latency, fps, CPU (browser and
      server), bitrate in its results log; fix the worst finding.
- [ ] Delete or archive `spikes/`; keep `docs/spikes/`.

## Risks

- **Chrome 155 pathologies** (2D-canvas frames to the hardware encoder from a Worker; paced
  hardware encode on the main thread): avoided by design, root cause unknown; re-run
  `spikes/webcodecs` after Chrome updates.
- **Lip sync**: AAC priming (43–64 ms) and per-frame camera transfer; measure with a clap in
  M1 before building on top.
- **Platform quirks**: `rml_rtmp`'s FP9 handshake, `flashVer` and object-shaped `onMetaData`
  are untested on Twitch/YouTube; budget 600–700 lines for a rewrite behind the same API.
- **Real networks**: all backpressure numbers are loopback; bufferbloat and loss change the
  thresholds (M6).
- **Hardware encoder drops frames silently** under saturation (2 of 90 at 360p); output
  counting is mandatory.
- **No TURN**: some guests will not connect over the mesh; Phase 2's SFU removes it.
- **Measurements taken under load** (7–19 on 10 cores); repeat when idle before quoting.
- **CI unverified** until the first push.

## Out of scope for Phase 0

Accounts, billing, SFU, plugins, themes, design system, database, Kubernetes, TURN, MP4
remux, WHIP ingest, WebGPU by default, platform OAuth, Linux producers (if option A).
