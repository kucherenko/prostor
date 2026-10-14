# ADR 0004: RTMP egress is built on `rml_rtmp` behind a sans-IO publisher

- Status: proposed (from spike 0002; accept after the first Twitch/YouTube publish)
- Date: 2026-10-14

## Context

RTMP never originates in the browser, so the server needs an RTMP client. The brief asks to
evaluate `rml_rtmp` (sans-IO) before writing our own. Spike 0002 published a 6 s H.264/AAC
sample through it to ffmpeg's RTMP server: handshake to "publishing" in 1.7 ms, 466 of 466
media tags received intact. `rml_rtmp` 0.8.0 is unmaintained since 2023-04-29, pulls old
`sha2`/`hmac`/`rand` majors, reads `SystemTime::now()` (native only), has no RTMPS and no
reconnect logic, and sends `onMetaData` as an AMF0 object rather than an ECMA array. A
replacement would be roughly 600–700 lines (handshake, chunking, AMF0 commands, ack window).

## Decision

1. `prostor_protocol::rtmp::Publisher` wraps `rml_rtmp`'s handshake and `ClientSession` as a
   sans-IO state machine (`new`, `handle_input`, `publish_metadata`, `publish_tag`, `stop`),
   compiled only for native targets. The server never touches `rml_rtmp` directly.
2. The server defines `trait Egress { start, push(tag), stop }` with `RtmpEgress` (ours,
   TCP via tokio, RTMPS via `tokio-rustls`), `FileEgress` (recording) and `FfmpegEgress`
   (fallback behind the same trait, not on the hot path unless the Rust path stalls).
3. An egress supervisor per room caches the `onMetaData` tag and the AVC/AAC sequence
   headers, runs one task and one bounded queue per destination, reconnects with backoff
   (resend headers, resume at the next keyframe) and emits per-destination status events.
4. Stream keys live in memory only, wrapped in a redacted-`Debug` secret type, never logged
   or persisted.

## Consequences

- A working egress in hours rather than days; swapping the RTMP implementation later does
  not touch the supervisor.
- If Twitch or YouTube reject the FP9 handshake, the `flashVer` string or the object-shaped
  `onMetaData` (pending a stream key), the fix is either a patch to a vendored copy or the
  600–700 line rewrite behind the same `Publisher` API.
- `SetPeerBandwidth` and `onBWDone` are surfaced as unhandled events and ignored.
- Exception to ADR 0001: `rml_rtmp` reads the system clock and the RNG internally, so
  `prostor_protocol::rtmp` is native-only (`cfg(not(target_arch = "wasm32"))`) and its
  output is not deterministic. Tests drive it against an in-process `rml_rtmp` server
  session instead of comparing bytes.
