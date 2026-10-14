# ADR 0003: Ingest transport is a binary WebSocket with application-level acks

- Status: proposed (from spike 0004; to be accepted with the Phase 0 plan review)
- Date: 2026-10-14

## Context

The producer's browser must push ~6 Mbit/s of FLV tags to the server with low, bounded
latency. Spike 0004 showed that a WebSocket carries this at 1–4 ms on loopback and that the
server's receive path is not a bottleneck (4 Gbit/s per connection). The real problem is
visibility: when the uplink or the server falls behind, about 2.3 MB (3 s of video) queue in
kernel and renderer buffers before `bufferedAmount` or `WebSocketStream` report anything, and
every policy driven by those APIs settles at 4–6 s of latency. A policy driven by a server
ack bounded latency at p95 ≈ 0.6 s.

## Decision

1. Transport: classic binary `WebSocket` to `/ws/ingest/<room>`, opened and written from the
   encoder Worker (main-thread timers throttle in background tabs). Not `WebSocketStream`,
   not WebTransport, in Phase 0.
2. Framing: one FLV tag per message, prefixed with a `u32` sequence number. No batching
   (5 tags per message cost +135 ms p95 for no CPU gain).
3. Acks: the server sends `{seq, server_ms}` of the newest tag handed to egress every
   100 ms. The browser's lag is the age of the oldest unacked tag (0 when nothing is
   outstanding).
4. Policy in the browser: lag > 500 ms → drop non-key video tags until the next keyframe,
   request a keyframe, lower the encoder bitrate by 25 % (raise slowly while lag < 100 ms);
   lag > 2 s → show "degraded"; lag > 10 s or no ack for 5 s → reconnect. Audio tags are
   never dropped. `bufferedAmount` is only a secondary UI indicator.
5. Session: destinations and stream keys travel over the signaling socket in a
   `start_stream` message; the ingest socket authenticates with the room's producer token;
   stopping sends an explicit `end_of_stream` and waits for its ack.
6. Server: a bounded queue (~2 s) per destination; on overflow drop non-key video for that
   destination until its next keyframe, never audio; per-destination lag, drops and
   reconnects are reported to the producer over the signaling socket.

## Consequences

- Latency under a slow uplink stays below ~1 s instead of growing without bound, at the cost
  of dropping ~20 % of frames during a 33 % overshoot.
- The ack cadence adds up to 100 ms of detection delay; tune once the protocol exists.
- Thresholds were measured on loopback only; they must be validated on a real remote server
  and with `dummynet` before the acceptance test (plan M6).
