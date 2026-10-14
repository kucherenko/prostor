# ADR 0005: Encoder configuration, threading model and the AAC strategy

- Status: proposed (video part from spike 0001; the AAC part needs the maintainer's decision)
- Date: 2026-10-14

## Context

RTMP destinations want H.264 + AAC with a keyframe every ≤ 2 s. Spike 0001 (Chrome 155,
macOS, M1 Pro) measured hardware H.264 at 1080p30 in a Worker at 11.5 ms mean latency and
15–23 % of one core, software H.264 at about one core, AAC-LC available with a 2-byte
`AudioSpecificConfig`, and two Chrome pathologies: GPU-backed 2D-canvas frames fed to the
hardware encoder from a Worker collapse to 13 fps, and a paced hardware encoder on the main
thread lags by seconds. `MediaStreamTrack` is not transferable to a Worker and
`MediaStreamTrackProcessor` is Window-only. Chromium on Linux ships no AAC encoder.

## Decision

1. Video: `avc1.4d0028` (Main 4.0; Baseline 3.1 fails level validation at 1080p),
   `avc: { format: "avc" }`, `latencyMode: "realtime"`, `bitrateMode: "constant"`, 30 fps,
   keyframe forced every 60 frames via `encode(frame, { keyFrame: true })`.
   `hardwareAcceleration: "prefer-hardware"` first, software fallback on unsupported or
   error, with the active encoder shown in the UI.
2. Threading: the compositor Worker renders with WebGL2 and hands its `VideoFrame`s to the
   encoder in the same Worker. Never 2D-canvas frames, never encoding on the main thread.
   Camera and screen-share frames are read on the main thread with
   `MediaStreamTrackProcessor` and transferred per frame (0.3 ms each). Audio stays in Web
   Audio on the main thread and feeds an `AudioEncoder`.
3. Timestamps: both encoders' `chunk.timestamp` (µs, one `performance.now()` origin) are the
   source of truth; the muxer rounds to FLV milliseconds. The AAC encoder's priming delay
   (2–3 chunks, 43–64 ms) is measured once and compensated.
4. Health: steady state is `encodeQueueSize ≤ 1`; warn when it stays > 2, drop non-key
   frames before `encode()` when > 8; dropped = submitted − output − queued, because
   hardware encoders drop silently under saturation.
5. Audio codec: AAC-LC 48 kHz stereo 128 kbit/s from WebCodecs on macOS and Windows,
   `decoderConfig.description` copied verbatim into the FLV sequence header.
6. Linux producers — **maintainer's choice**:
   - (A, recommended for Phase 0) support macOS/Windows producers only; add Opus → AAC on
     the server in Phase 1, most likely as an ffmpeg sidecar process (LGPL, separate
     process, no linking into our binary);
   - (B) add the server-side Opus → AAC path now (ffmpeg sidecar, or `fdk-aac` bindings
     with their own license terms), which also means carrying Opus in FLV.

## Consequences

- The encoder costs ~0.2 core (hardware) or ~1 core (software) on this laptop; the
  compositor and up to four WebRTC decodes get the rest.
- Main-thread jank delays camera frames; the UI must stay light while live.
- Both Chrome pathologies are unexplained; re-run `spikes/webcodecs` after Chrome updates.
