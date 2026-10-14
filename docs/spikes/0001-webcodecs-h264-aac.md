# Spike (a): WebCodecs H.264 + AAC on macOS Chromium

**Answer: the browser-first encoder path works on this laptop with large margins, as long as
encoding happens in a Worker fed with WebGL2 frames.** Hardware H.264 (VideoToolbox) encodes
1920×1080 @ 30 fps with 11.5 ms mean latency using 15–19 % of one core for the whole Chrome
process tree; the software fallback (OpenH264) needs about one core. AAC-LC is available and
yields exactly what the FLV muxer needs (raw frames plus a 2-byte AudioSpecificConfig).
Two Chrome 155 pathologies were found and are avoided by the planned design: GPU-backed 2D
canvas frames in a Worker starve the hardware encoder, and real-time paced hardware encoding
on the main thread lags by seconds.

- Code: `spikes/webcodecs/` (self-contained page + puppeteer-core runner).
- Raw results: `spikes/webcodecs/results/*.json`.
- Date: 2026-10-14. Chrome 155.0.8059.40, macOS 26.6.2, Apple M1 Pro (10 cores).

## Quick path (reproduce, also on Linux/Windows)

1. `cd spikes/webcodecs && pnpm install && node run.mjs` (headless, waits for an idle
   machine, writes `results/<timestamp>-headless.json`). `--headed` opens a window.
2. Without the runner: `node serve.mjs`, open `http://127.0.0.1:9472/index.html` in Chrome,
   press **Run**, wait ~3 minutes, press **Copy JSON**. Paste the JSON into a new results
   file and add a row to the tables below. **Re-run this on every OS you produce from**;
   AAC availability and hardware encoding differ per OS.
3. Useful variants: `--sources=canvas2d,webgl2` (frame-source comparison),
   `--phases=caps,audio` (fast capability check), `--hw=sw` (software only).

## Setup

| Item | Value |
| --- | --- |
| Machine | MacBook Pro 18.1, Apple M1 Pro, 10 cores, macOS 26.6.2 |
| Browser | Google Chrome 155.0.8059.40, `--headless=new` (2 runs) and headed (1 run); hardware video encode active in both modes |
| chrome://gpu | Video Encode: hardware accelerated; WebGL, WebGPU: hardware accelerated; ANGLE Metal |
| Flags | `--use-fake-device-for-media-stream=fps=30 --use-fake-ui-for-media-stream` |
| Load | **Machine was not idle**: 1-min load average 7–8.5 during both headless runs (other agents compiling in parallel; the 10-minute idle wait timed out). CPU numbers are therefore conservative. The two runs agree within noise. |
| Video config | 1920×1080, 30 fps, 6 Mbit/s, `latencyMode: realtime`, `bitrateMode: constant`, `avc: { format: "avc" }`, keyframe forced every 60 frames, 600 frames (20 s) |
| Audio config | 48 kHz stereo 128 kbit/s, 1024-sample `AudioData` chunks, 20 s |

## Results

### Capability matrix (1080p30, 6 Mbit/s, realtime)

| Codec string | Profile/level | hw: no-preference | prefer-hardware | prefer-software | CBR / VBR |
| --- | --- | --- | --- | --- | --- |
| `avc1.42001f` | Baseline 3.1 | **no** (level too low for 1080p30) | no | no | – |
| `avc1.420028` | Baseline 4.0 | yes | yes | yes | both |
| `avc1.4d0028` | Main 4.0 | yes | yes | yes | both |
| `avc1.640028` | High 4.0 | yes | yes | yes | both |
| `avc1.4d0028` + `annexb` | Main 4.0 | yes | – | – | – |
| `vp09.00.40.08`, `av01.0.08M.08`, `hvc1.1.6.L120.B0` | informational | yes | – | – | – |

| Audio | Supported | Normalized config |
| --- | --- | --- |
| `mp4a.40.2` (AAC-LC), `aac: { format: "aac" }` | **yes** | bitrateMode `variable` |
| `mp4a.40.2`, `aac: { format: "adts" }` | yes | – |
| `mp4a.40.5` (HE-AAC) | yes | – |
| `opus` | yes | bitrateMode `variable` |

Environment: `VideoEncoder`, `AudioEncoder`, `VideoFrame`, `OffscreenCanvas`,
`WebSocketStream`, `WebTransport`, `navigator.gpu` exist in both Window and Worker.
`MediaStreamTrackProcessor` exists **only in Window** in Chrome 155.

### 1080p30 throughput (WebGL2 frame source, Main 4.0, two headless runs)

| Run | fps | Encode latency mean / p95 / max (ms) | Queue max (frames with queue > 2) | kbit/s | Key / P size | Chrome CPU mean / max (% of one core) |
| --- | --- | --- | --- | --- | --- | --- |
| **Worker, HW, paced 30 fps** | 30.0 | **11.4–11.7 / 15.0–15.3 / 43–49** | 1 (0) | 5743 | 13.8 KB / 24.1 KB | **15–19 / 55** |
| Worker, HW, unpaced | 160.6–161.4 | 77 / 83–85 / 121–132 | 8 (598) | 5743 | | 39–43 / 58 |
| Worker, SW, paced 30 fps | 30.0 | 13.8–13.9 / 15.4–15.7 / 59–67 | 1 (0) | 5994 | 32.6 KB / 24.8 KB | 95–96 / 119 |
| Worker, SW, unpaced | 94.7–97.9 | 128–133 / 145–154 / 166–182 | 8 (598) | 5994 | | 293–295 / 299 |
| Main, HW, paced 30 fps | 30.0 | **1432–1607 / 6029–6440 / 6822–7238** | **202–212** (248–261) | 5743 | | 22–24 / 71 |
| Main, HW, unpaced | 159.9–161.3 | 77 / 83–86 / 96–106 | 8 (598) | 5743 | | 43–44 / 53 |
| Main, SW, paced 30 fps | 30.0 | 13.5–13.9 / 15.0–15.6 / 54–59 | 1 (0) | 5994 | | 94 / 108 |
| Main, SW, unpaced | 96.6–98.8 | 127–130 / 137–146 / 154–169 | 8 (598) | 5994 | | 286–294 / 302 |

Common to all runs: 600/600 frames out, 10 keyframes (one every 60 frames, exactly as
requested), output `chunk.timestamp` equal to input for every frame, in order, no errors.
`metadata.decoderConfig.description` arrived with the first chunk, once: 35 bytes from the
hardware encoder (`01 4d 00 28 ff e1 00 14 27 4d 00 28 …`), 34 bytes from the software
encoder (`01 4d 4c 28 ff e1 00 13 67 4d 4c 28 …`); both are valid AVCDecoderConfigurationRecords
(version 1, profile 0x4d, level 0x28) and can be copied verbatim into the FLV video sequence
header. Hardware output ran 4 % under the target bitrate; software hit it. Frame generation
cost: WebGL2 0.1 ms, GPU 2D canvas 0.4 ms, `getImageData` RGBA 3.8 ms, CPU 2D canvas 7.3 ms.

### Frame source matters (120 frames, unpaced, queue cap 8, load ≈ 12)

| `VideoFrame` source | Worker + HW fps (latency mean / p95) | Main + HW fps | Worker + SW fps |
| --- | --- | --- | --- |
| 2D canvas, GPU-backed (default) | **13.6 (616 ms / 8130 ms)** | 157 | 88 |
| 2D canvas, `willReadFrequently` (CPU) | 129 (74 / 93) | 122 | 73 |
| RGBA buffer via `getImageData` | 148 (76 / 91) | 146 | 78 |
| **WebGL2 canvas** | **157 (75 / 91)** | 157 | 78 |

The smoke run reproduced the first row when paced (9.8 fps, queue 60). Only the combination
Worker + GPU-backed 2D canvas + hardware encoder is affected.

### AAC and Opus (20 s, 48 kHz stereo, 128 kbit/s)

| | AAC-LC (`aac` format) | Opus |
| --- | --- | --- |
| Inputs → chunks | 938 → 941 | 938 → 1001 |
| Bitrate | 131.1 kbit/s | 128.7 kbit/s |
| `description` | 2 bytes `11 90` (AAC-LC, 48 kHz, stereo), with first chunk | 19 bytes `OpusHead` v1, 2 ch, pre-skip 312 samples |
| Chunk duration | 21333 / 21334 µs alternating (exact 1024-sample grid) | 20000 µs |
| Chunk size | 6–421 bytes, mean 348 (first chunks are priming) | 289–493, mean 321 |
| Output timestamps | start at the first input timestamp, then +1024 samples per chunk regardless of input timestamps | same, on a 960-sample grid |
| Tail | 3 extra chunks (≈ 3072 samples: encoder priming + padding) | 1 extra chunk |
| `bitrateMode: constant` | byte-identical to `variable` (ignored) | – |
| Encode wall time | 172 ms for 20 s (≈ 1 % of a core) | 254 ms |

### A/V clock (one `performance.now()` origin in µs for both encoders)

- Video: first output timestamp equals the origin; all 1080p outputs preserve input
  timestamps exactly. At 640×360 unpaced with `no-preference` the encoder returned
  **88 of 90 frames** in every run: hardware encoders can drop frames silently when the
  queue is saturated.
- Audio: output timestamps are re-derived from the first input on the exact sample grid; the
  per-chunk rounded input timestamps are not echoed back. Monotonic in all runs.

### Camera to Worker (fake 1080p30 device, 300 frames)

| Path | Result |
| --- | --- |
| Transfer `MediaStreamTrack` to Worker | **Not supported**: `DataCloneError: … does not have a transferable type`; with `--enable-blink-features=MediaStreamTrackTransfer`: `MediaStreamTrack could not be serialized`. Moot anyway: no `MediaStreamTrackProcessor` in Worker scope. |
| Processor on main thread, `postMessage(frame, [frame])` to Worker | **Works**: 300/300 frames at 30.1 fps; `postMessage` 0.06 ms; transfer latency mean 0.24–0.42 ms, p95 0.4–0.6 ms, max 1–5 ms; encode latency (HW) mean 8.4–9.7 ms; Chrome CPU ≈ 10 %. |

### Headless vs headed

All three runs at 30 fps paced, latency as mean / p95 / max in ms, machine at load ≈ 8.

| Mode | Worker + HW (CPU) | Worker + SW (queue max) | Main + HW (queue max) | Main + SW (queue max) | Camera transfer mean / p95 |
| --- | --- | --- | --- | --- | --- |
| Headless 1 | 11.7 / 15.3 / 43 (19 %) | 13.8 / 15.7 / 67 (1) | 1432 / 6029 / 6822 (202) | 13.9 / 15.6 / 59 (1) | 0.24 / 0.4 ms |
| Headless 2 | 11.4 / 15.0 / 49 (15 %) | 13.9 / 15.4 / 59 (1) | 1607 / 6440 / 7238 (212) | 13.5 / 15.0 / 54 (1) | 0.42 / 0.6 ms |
| Headed | **10.9 / 13.9 / 45 (23 %)** | 31.1 / 151.9 / 439 (9) | 1400 / 5953 / 6744 (200) | 364 / 2233 / 2714 (85) | 0.95 / 1.1 ms |

The main-thread hardware stall is not a headless artifact. With a visible window the
software encoder's p95 rose 10× in the Worker and 150× on the main thread, and the WebGL2
draw on the main thread went from 0.08 ms to 2.1 ms, while hardware encoding in the Worker
did not move. Capabilities, AAC/Opus, clock and camera results were identical in all modes.

## Conclusions for Phase 0

| Topic | Decision / recommendation |
| --- | --- |
| Codec string | `avc1.4d0028` (Main 4.0). Level 4.0 is the minimum that passes `isConfigSupported` at 1080p30; Baseline 3.1 is rejected. High 4.0 works too; Main is the safer RTMP default. |
| Encoder config | `avc: { format: "avc" }`, `latencyMode: "realtime"`, `bitrateMode: "constant"`, `framerate: 30`, keyframe via `encode(frame, { keyFrame: true })` every 60 frames (2 s), which the encoders honor exactly. Copy `decoderConfig.description` verbatim into the FLV AVC sequence header; it arrives once, with the first chunk. |
| Hardware vs software | `prefer-hardware` first (11.5 ms, 0.15–0.23 core, ceiling 160 fps, identical headed and headless); fall back to `prefer-software` when unsupported or on an `error` callback (14 ms, ≈ 1 core at 30 fps, ceiling ≈ 95 fps with 3 cores). Software is a fallback, not a peer: with a visible window on a loaded machine its p95 reached 152 ms in the Worker. Show which encoder is active in the UI. |
| CPU budget (this laptop) | Encoder 0.2 core (HW) or 1.0 core (SW), AAC 0.01 core, camera frame transfer 0.1 core. The compositor and four WebRTC decodes get the rest. |
| Threading | **Encode in the Worker and feed it WebGL2-rendered `VideoFrame`s.** Never hand GPU-backed 2D-canvas frames from a Worker to the hardware encoder (13 fps, seconds of latency), and do not run a paced hardware encoder on the main thread (1.4–1.6 s mean latency, queue 200+, reproduced headed and headless). Both behaviors are Chrome 155/macOS observations with unknown root cause; re-check after Chrome upgrades with `--sources=canvas2d,webgl2`. |
| Camera path | Read the camera with `MediaStreamTrackProcessor` on the main thread and transfer each `VideoFrame` to the Worker (0.3 ms per frame). The main thread must stay responsive; UI jank delays frames. Audio stays in Web Audio on the main thread. |
| Dropped-frames UI | Healthy steady state is `encodeQueueSize ≤ 1`. Flag "encoder behind" when the queue stays > 2 for 5 consecutive frames; drop non-key frames before `encode()` when it exceeds 8. Count outputs too: dropped = submitted − output − queued, because encoders can drop silently. |
| AAC on macOS | Available via AudioToolbox: AAC-LC 48 kHz stereo, raw frames, `description` = 2-byte AudioSpecificConfig → FLV AAC sequence header. `bitrateMode` is ignored. |
| AAC on Linux | Expected **unavailable** (Chromium ships no AAC encoder there); verify with this page. The maintainer's decision: (A) Phase 0 supports macOS/Windows producers only and defers Opus → AAC to Phase 1, or (B) add server-side Opus → AAC now (ffmpeg sidecar `-c:a aac` under LGPL, or `fdk-aac` with its own license terms). Opus output from WebCodecs is clean (20 ms chunks, `OpusHead`), so the input side of a transcode is ready either way. |
| Timestamps for the muxer | Use `chunk.timestamp` (µs) from both encoders, never a local counter; convert to FLV milliseconds with rounding (±0.5 ms jitter, no drift). Audio content lags its timestamps by the AAC encoder priming (2–3 chunks, 43–64 ms); measure it precisely and subtract a constant, or accept it (borderline for lip-sync). One `performance.now()` origin for both clocks works. |
| CI | Headless Chrome on macOS has hardware encode, so encoder tests can run headless on a Mac runner. Linux runners have OpenH264 and no AAC. |

## Open questions / risks

- [ ] Measurements were taken on a loaded machine (load 7–8 on 10 cores); repeat on an idle
      one before quoting CPU numbers externally. The first headed attempt died in the camera
      phase (page frame detached); the runner now saves partial results on failure.
- [ ] Root cause of the two stalls (2D canvas in Worker + HW; main thread paced + HW) is not
      investigated. Consider an upstream report once reproduced on a clean profile.
- [ ] Exact AAC encoder delay: measure with an `AudioDecoder` round trip of a click track,
      then confirm end to end with the stopwatch method in `docs/acceptance/phase0.md`.
- [ ] Silent frame drops by the hardware encoder under saturation (2 of 90 at 360p): find
      the trigger and whether `latencyMode: "quality"` avoids it.
- [ ] Windows (Media Foundation) and Linux (OpenH264 / VA-API, no AAC) not measured.
- [ ] Hardware rate control: 4 % under target here; check behavior across long runs and
      scene cuts, since bitrate spikes feed the WebSocket backpressure design (spike d).
- [ ] `MediaStreamTrackProcessor` is Window-only in Chrome 155; revisit when it moves to
      Workers (then transfer the track and skip the per-frame hop).
