# Spike 0004: binary WebSocket ingest at 6 Mbit/s and where backpressure hides

**Answer first.** A binary WebSocket carries 6 Mbit/s from Chrome 155 to an axum server with
~1–4 ms one-way latency, and the server's receive path has a ceiling above 4 Gbit/s per
connection (29 % of one core). The problem is not throughput, it is *visibility*: when the
server (or the uplink) falls 2 Mbit/s behind, about **2.3 MB (≈3 s of video) queue up below
the browser APIs** before `bufferedAmount` or `WebSocketStream` report anything, and one-way
latency silently climbs past 2 s. Every policy driven by `bufferedAmount` settles at 4–6 s
of latency; a policy driven by a **server ack (age of the oldest unacknowledged frame)**
bounds it at **p95 ≈ 0.6 s** while dropping ~20 % of frames (delta frames only). Phase 0
should therefore put a periodic ack into the ingest protocol and derive drop and bitrate
decisions from it, send one FLV tag per message, and keep classic `WebSocket`.

- Code: `spikes/ws-backpressure/` (axum sink + browser page + puppeteer-core runner).
- Raw results: `spikes/ws-backpressure/results/*.json` (one file per scenario run).
- Date: 2026-10-14. Machine: Apple M1 Pro (10 cores), macOS 26.6, Chrome 155.0.8059.40
  (`--headless=new`), Rust 1.93 / axum 0.8.9 / tungstenite 0.29, loopback only.

## Question

1. Can the browser push 6 Mbit/s of FLV-sized binary frames over a WebSocket to our server
   with low latency, and what does it cost in CPU?
2. When the consumer is slower than the producer, how fast and how visibly does backpressure
   reach the page, and which drop policy keeps latency bounded?
3. Is `WebSocketStream` (Chrome's streams-based API) a better backpressure source?
4. Does batching several tags per message help? Does anything justify WebTransport?

## Setup

| Part | What it does |
| --- | --- |
| Server (`src/main.rs`) | `GET /ws/ingest?id&rate_bps&slow_ms&ack_ms` binary sink. Parses records (`u32 len`, `u8 kind` key/delta, `u32 seq`, `f64 Date.now()`, payload). Records bytes/s, one-way latency (server clock − client stamp, same machine), inter-arrival gaps, sequence gaps. `rate_bps` makes it consume at a fixed bit rate (sleep per message); `slow_ms` sleeps a fixed time per message. `POST /limit` changes a live connection's limit. `ack_ms` sends `{seq, sent_ms, server_ms}` of the newest consumed frame every N ms. `--rcvbuf` sets `SO_RCVBUF` on the listener. |
| Page (`web/sender.js`) | Sends synthetic frames on a `setTimeout` chain with drift correction at 30 fps: GOP of 60 frames, keyframe = 6 × delta, sizes solved so the mean is 6 Mbit/s (delta 23 046 B, key 138 462 B). Samples `bufferedAmount` every 50 ms, polls `/stats` every 500 ms to compute *hidden bytes* = sent − `bufferedAmount` − received by the server. Drop policies `none`, `drop-delta`, `drop-all`; pressure signal `buffered` (`bufferedAmount` > threshold) or `ack` (age of the oldest unacked frame > `lagMs`). `mode=wsstream` uses `WebSocketStream` and measures how long `writer.write()` promises take. |
| Runner (`web/run.mjs`) | Starts the server, drives the page in headless Chrome, samples CPU of the server and of the Chrome process tree from `ps` every 500 ms, applies CDP throttling for S7, runs the Node blast client for S8, writes JSON + `summary.json`. |

Kernel defaults on this machine: `net.inet.tcp.recvspace` = `sendspace` = 131 072,
`autorcvbufmax` = `autosndbufmax` = 4 194 304 (autotuning on). The listener reports
`SO_RCVBUF` = 131 072.

Measurement caveat: the machine was busy during the runs (1-minute load average 8–19 from
the sibling spikes, Docker and the user's own Chrome; recorded per result as `idle.loadavg1`).
Loopback queueing results are buffer-dominated and reproduced within ~5 % across the repeated
runs (S1, S2, S3a, S3d twice each); CPU figures are indicative only.

## How to reproduce

```sh
cargo build -p ws-backpressure --release
cd spikes/ws-backpressure/web && pnpm install
node run.mjs                      # all scenarios (~15 min), results in ../results/
node run.mjs --scenario S2,S3d    # a subset; --skip-idle to ignore the load gate
```

Manual: `target/release/ws-backpressure`, then open
`http://127.0.0.1:9471/?rate_bps=4000000&policy=drop-delta&signal=ack`.

## Results

### Baseline and ceilings

| Scenario | Sent | Received | `bufferedAmount` max | One-way latency mean / p95 / max | CPU server / Chrome tree |
| --- | --- | --- | --- | --- | --- |
| S1 baseline, run 1 | 6.04 Mbit/s | 6.03 Mbit/s | 138 KB (one keyframe) | 1.3 / 3.7 / 28 ms | < 1 % / 5 % |
| S1 baseline, run 2 | 6.04 | 6.03 | 138 KB | 1.0 / 1.8 / 16 ms | < 1 % / 5 % |
| S9 batch = 5 frames per message | 6.04 | 6.03 | 138 KB | 68 / 135 / 141 ms | < 1 % / 3 % |
| S4 limit 8 Mbit/s (just above mean), keyframe bursts | 6.04 | 6.01 | 138 KB | 15.5 / 92 / 162 ms | < 1 % / 4 % |
| S8a one Node client, 1 GB, 25 KB frames | 4 404 Mbit/s | **4 225 Mbit/s** | – | p95 1.0 ms | **29 % mean, 33 % max** |
| S8b four Node clients, 1 GB each | 4 × ~4 100 | **15 142 Mbit/s** | – | p95 1.2 ms | 113 % mean, 153 % max |

Timer jitter of the `setTimeout` chain in the (foreground, headless) page: p95 2.7 ms, max 3.8 ms.
Server CPU at 6 Mbit/s is below the sampler's resolution (< 1 % of a core); from S8a the
receive path costs ≈ 7 µs of CPU per 25 KB frame.

### Slower consumer, no policy (what the browser sees)

| Scenario | Sent / received | `bufferedAmount` first > 2 keyframes at | Growth once visible | Hidden bytes at onset / max | Latency mean / p95 / max | Drain after stop |
| --- | --- | --- | --- | --- | --- | --- |
| S2 server limit 4 Mbit/s, run 1 | 6.04 / 3.84 | 5.4 s | 328 KB/s | 1.46 / 2.29 MB | 8.6 / 16.2 / 16.9 s | 12.9 s |
| S2, run 2 | 6.04 / 3.85 | 8.5 s | 319 KB/s | 2.09 / 2.31 MB | 8.5 / 16.2 / 16.8 s | 12.8 s |
| S2b same, server `SO_RCVBUF` = 64 KiB | 6.04 / 3.85 | 4.1 s | 337 KB/s | 1.00 / 1.23 MB | 8.5 / 16.1 / 16.8 s | 14.8 s |
| S5a server sleeps 50 ms per message | 6.05 / 3.87 | 5.9 s | 322 KB/s | 1.23 / 2.29 MB | 8.4 / 15.9 / 16.8 s | 12.4 s |
| S6a `WebSocketStream`, no policy | 6.04 / 3.85 | 6.0 s | 328 KB/s | 1.39 / 2.26 MB | 8.5 / 16.2 / 16.8 s | 12.6 s |

S2 timeline (run 1): server-side latency mean was 335 ms in second 1, 928 ms in second 3,
2 010 ms in second 6, while `bufferedAmount` stayed at 23 KB (one delta frame) until
t = 5.5 s. Chrome never raised `error` or `close` while 6 MB were queued; the close after
the 12.9 s drain completed with code 1006 (abnormal), see open questions. In S6a the
`writer.write()` promises took 4.8 s mean / 11.7 s p95 to resolve with up to 241 writes
pending and `desiredSize` down to −239: the stream API reports the renderer queue, nothing
below it.

### Slower consumer with a drop policy (limit lifted at t = 20 s, 40 s runs)

| Scenario | Signal / threshold | Sent / received | `bufferedAmount` max | Hidden max | Ack lag p95 | Latency mean / p95 / max | Dropped (of 1 200) | Latency 1 s after lift |
| --- | --- | --- | --- | --- | --- | --- | --- | --- |
| S3a drop-delta | `bufferedAmount` > 750 KB | 5.51 / 5.51 | 900 KB | 2.27 MB | 5.2 s | 1.44 / 5.09 / 5.92 s | 113 | 1 ms |
| S3b drop-all | `bufferedAmount` > 750 KB | 5.36 / 5.35 | 847 KB | 1.67 MB | 4.3 s | 1.13 / 4.00 / 4.51 s | 146 | 1 ms |
| S3c drop-delta | `bufferedAmount` > 100 KB | 5.38 / 5.37 | 254 KB | 2.10 MB | 4.5 s | 1.22 / 4.34 / 4.55 s | 142 | 1 ms |
| **S3d drop-delta** | **ack lag > 500 ms** | 4.96 / 4.96 | 138 KB | 346 KB | 0.73 s | **0.14 / 0.66 / 0.78 s** | 232 (run 2: 234) | 1 ms |
| **S3e drop-all** | **ack lag > 500 ms** | 4.67 / 4.67 | 138 KB | 323 KB | 0.62 s | **0.15 / 0.61 / 0.70 s** | 295 | 1 ms |
| S5b drop-delta, 50 ms/message consumer | `bufferedAmount` > 750 KB | 4.74 / 3.97 | 908 KB | 2.29 MB | 6.0 s | 4.41 / 5.89 / 6.07 s | 211 | – |
| S5c drop-delta, 50 ms/message consumer | ack lag > 500 ms | 3.70 / 3.69 | 138 KB | 300 KB | 1.3 s | 0.35 / 0.67 / 0.72 s | 380 | – |
| S6b `WebSocketStream` drop-delta | pending write | 4.45 / 3.86 | 162 KB | 2.15 MB | 4.4 s | 3.37 / 4.33 / 4.49 s | 260 | – |

In S3d the server-side latency per second stayed between 270 and 465 ms for the whole
limited phase and fell to 1 ms within one second of lifting the limit; no keyframe was ever
dropped. In S3a it climbed to 5.6 s and needed the lift to recover. The first version of the
ack signal (age of the newest *acked* frame) deadlocked drop-all: once the page stops
sending, the server has nothing to ack and the stale age grows forever. The age of the
oldest *unacked* frame (0 when the pipe is empty) is the correct signal.

### CDP throttling (S7)

With `Network.emulateNetworkConditions` `uploadThroughput` = 4 Mbit/s and no server limit,
the server still received **6.04 Mbit/s** (808 / 693 KB per second alternating with the
keyframe pattern). Chrome 155's network emulation does **not** throttle WebSocket frames;
it only added jitter (latency mean 57 ms, p95 232 ms). Uplink bottlenecks must be
emulated with a real server-side limit (as here) or with `dummynet`/`tc netem`.

## Where the bytes hide

At a 2 Mbit/s overshoot the invisible queue filled at ≈ 250 KB/s and saturated at
2.2–2.3 MB (≈ 3 s of video) before `bufferedAmount` moved. Forcing the server's receive
buffer to 64 KiB removed ≈ 1.1 MB of it (S2b: 1.23 MB left), so on loopback roughly half
is the server's autotuned kernel receive buffer and the other ≈ 1.2 MB sits on the browser
side: Chrome's renderer → network-service pipe and the kernel send buffer, both below
`bufferedAmount` and below `WebSocketStream`'s write promises. Shrinking the server buffer
did not help latency at all (16.8 s in both cases); it only moved the queue into the page.
On a real path the uplink router's queue (bufferbloat) adds to this and is equally
invisible. Only an application-level ack measures the real queueing delay.

## Conclusions for Phase 0

| Topic | Decision |
| --- | --- |
| Pressure signal | Add a periodic server ack to the ingest protocol (every 100 ms, or per consumed keyframe): the newest tag the server has *handed to egress* plus its server time. The browser keeps send times of outstanding tags and uses the age of the oldest unacked tag as the lag. Treat `bufferedAmount` only as a secondary, UI indicator. |
| Drop policy | `drop-delta` on lag > 500 ms: skip non-key video tags, never audio (128 kbit/s, independent frames, and gaps break A/V sync). Unlike this spike, a real H.264 stream cannot resume deltas after a drop: once a video tag is dropped, drop until the next keyframe and ask the encoder for one immediately. The same lag drives bitrate adaptation (lower the encoder target by ~25 % when lag > 500 ms, raise slowly while lag < 100 ms). |
| Thresholds | lag > 500 ms: drop deltas + request keyframe + lower bitrate; lag > 2 s: show "degraded"; lag > 10 s or no ack for 5 s: reconnect. UI: lag (ms), `bufferedAmount`, dropped frames, current bitrate. |
| `WebSocketStream` | No. It bounds the renderer-side queue (S6b: 2 pending writes max) but sees the same 2 MB blind spot (p95 4.3 s). Classic `WebSocket` is simpler and universal. |
| Message framing | One FLV tag per binary message, prefixed with a `u32` sequence number for acks. Batching 5 tags per message added 67 ms mean / 135 ms p95 latency for no measurable CPU gain (S9). |
| Sender placement | Send from the encoder Worker (the encoder's output callback drives sends), never from a main-thread timer: background tabs throttle main-thread timers. |
| Server with a slow RTMP destination | Server receive is not the bottleneck (4 Gbit/s per connection). Each destination gets its own task and a bounded queue (≈ 2 s of tags). On overflow: drop non-key video tags for that destination until its next keyframe, never audio, count drops; if the queue stays full for > 10 s, reconnect with backoff and mark the destination degraded. Report per-destination lag, drops and reconnects to the producer over the signaling socket, so one slow platform never stalls the others or the recording. |
| Stop sequence | End a session with an explicit end-of-stream message and wait for its ack rather than relying on the close handshake (code 1006 observed after a long drain). |
| WebTransport | Nothing measured justifies it for Phase 0: loopback WebSocket latency is 1–4 ms at 6 Mbit/s and the failure mode is queue visibility, which an ack fixes on any transport. Revisit only for lossy links where TCP retransmits stall the stream. |

## Checklist for the ingest adapter

- [ ] Ack message in the ingest protocol; lag = age of the oldest unacked tag, 0 when empty.
- [ ] Drop deltas until the next keyframe on lag > 500 ms; request a keyframe; audio always sent.
- [ ] Bitrate adaptation driven by the same lag.
- [ ] Sender runs in the encoder Worker; one tag per message with a sequence prefix.
- [ ] Status bar shows lag, `bufferedAmount`, dropped frames, bitrate.
- [ ] Server: bounded per-destination queues, drop-until-keyframe, reconnect, status reporting.
- [ ] Explicit end-of-stream + ack on stop.

## Open questions

- Real uplinks: bufferbloat and loss cannot be reproduced on loopback (CDP does not throttle
  WebSockets). Validate the thresholds on a real remote server and with `dummynet`
  (`pfctl`/`dnctl` on macOS) before the acceptance test.
- Ack cadence: 100 ms adds up to 100 ms of detection delay; a per-keyframe ack or a smaller
  period may suit better. Measure once the real protocol exists.
- Close code 1006 after a long drain (S2): the page closed while the server was still
  consuming the kernel backlog at 4 Mbit/s; whether Chrome's closing-handshake timeout or the
  rate-limited server caused it was not isolated.
- The server's own egress lag to each platform has the same blind spot (kernel send buffer to
  the RTMP endpoint). A bounded queue in front of the socket is the Phase 0 answer; measuring
  the socket's unsent bytes (`SO_NWRITE` on macOS, `SIOCOUTQ` on Linux) is a possible refinement.

## Next step

Fold the checklist into `docs/plan-phase0.md` (ingest adapter and egress supervisor items).
