# ADR 0002: Browser-first; the server does only what a browser cannot

- Status: accepted
- Date: 2026-10-14

## Context

A live-production studio needs capture, guest media, compositing, audio mixing, encoding,
muxing, multi-platform egress and recording. Doing all of that server-side (the StreamYard
model) needs heavy infrastructure per show. Chromium today exposes WebRTC, WebCodecs
(H.264 everywhere; AAC on Windows, macOS and Android), OffscreenCanvas/WebGL2, Web Audio
and WASM. The only thing a browser cannot do is open a TCP socket, and RTMP needs one.

## Decision

1. In the producer's browser: capture, guest media over a WebRTC peer-to-peer mesh (at most
   4 guests in Phase 0), compositing in a Worker on OffscreenCanvas, Web Audio mixing,
   WebCodecs encoding (H.264 + AAC, Opus fallback), FLV muxing in WASM, board and
   destination persistence in IndexedDB.
2. On the server: WebSocket signaling and presence, binary WebSocket FLV ingest, RTMP(S)
   egress fan-out with reconnect and per-destination status, FLV recording to disk, static
   hosting of the web app. Later: WHIP ingest, SFU, Opus→AAC transcoding.
3. Browser → server transport is binary WebSocket. WebTransport only if head-of-line
   blocking is measured to matter.
4. Chromium is the only supported producer browser in Phase 0; guests may use any modern
   browser.
5. Auth is a shared secret for the producer and room tokens for guests. Stream keys travel
   over WSS at stream start, live in memory only, are never logged or persisted. No
   database; rooms are in memory.

## Consequences

- The server is small, stateless apart from rooms, cheap to run, deployable as one binary.
- Output quality depends on the producer's machine and uplink, so the UI shows dropped
  frames, encoder queue depth and WebSocket buffered bytes from day one.
- Linux producers may lack WebCodecs AAC; the fallback (Opus plus server-side transcode)
  is a separate decision pending spike (a) results.
- Guest count is bounded by the mesh until an SFU exists (Phase 2).
