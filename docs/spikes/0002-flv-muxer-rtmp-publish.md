# Spike (b): FLV muxer in `prostor-protocol` and RTMP publish through `rml_rtmp`

**Outcome:** the sans-IO FLV muxer reproduces ffmpeg's audio and video tags byte for byte,
and a 6-second H.264/AAC sample published through `rml_rtmp` from a native binary arrived at
a local RTMP server with all 466 media tags intact. The publish path is ready for a real
platform; the Twitch/YouTube run is **pending a stream key**. Recommendation: adopt
`rml_rtmp` behind our own sans-IO `Publisher` for Phase 0 (details and caveats below).

- Date: 2026-10-14. Machine: Apple M1 Pro, macOS 26.6, rustc 1.93.0, ffmpeg 8.1,
  `rml_rtmp` 0.8.0. Load average during timings: 10–12 (other spikes were building).
- Code: `core/prostor-protocol/src/{flv,amf0,rtmp}.rs`, tests in
  `core/prostor-protocol/tests/flv_both_runtimes.rs`, binary in `spikes/rtmp-publish/`.

## Quick path (reproduce)

```sh
# 1. sample media (already committed: spikes/rtmp-publish/fixtures/testsrc-720p.flv, 1.6 MB)
ffmpeg -y -f lavfi -i "testsrc2=size=1280x720:rate=30" -f lavfi -i "sine=frequency=440:sample_rate=48000" \
  -t 6 -c:v libx264 -profile:v main -preset veryfast -g 60 -keyint_min 60 -x264-params scenecut=0 \
  -b:v 2000k -pix_fmt yuv420p -c:a aac -b:a 128k -ac 2 -f flv spikes/rtmp-publish/fixtures/testsrc-720p.flv

# 2. muxer conformance (native; the same tests run in Chrome, see spike 0003)
cargo test -p prostor-protocol

# 3. publish to a local RTMP server (ffmpeg as listener), then compare
cd spikes/rtmp-publish
ffmpeg -y -listen 1 -i rtmp://127.0.0.1:1935/live/spike -c copy out/received.flv &
cargo run -p rtmp-publish -- --input fixtures/testsrc-720p.flv --url rtmp://127.0.0.1:1935/live/spike --realtime
ffprobe -count_packets out/received.flv

# 4. publish to Twitch or YouTube (pending): the key goes through the environment, never argv
PROSTOR_SPIKE_RTMP_URL='rtmp://live.twitch.tv/app/<stream key>' \
  cargo run -p rtmp-publish -- --input fixtures/testsrc-720p.flv --realtime --loop 10
```

The binary prints the target with the key redacted (`rtmp://live.twitch.tv:1935/app/<redacted>`).

## Results

### FLV muxer vs ffmpeg (1-second fixture, `tests/fixtures/testsrc-720p-1s.flv`)

| Check | Result |
| --- | --- |
| Tags parsed | 1 script, 34 video (sequence header + 32 frames + end of sequence), 48 audio (sequence header + 47 frames) |
| Re-muxed audio/video tags identical to ffmpeg's bytes (header, body, PreviousTagSize) | 82 of 82 |
| Composition time offsets (B-frames, `has_b_frames=2`) | preserved, signed 24-bit verified in unit tests |
| Extended timestamps (> 16.7 M ms), oversized bodies, encrypted/unknown tags, odd chunk boundaries | covered by unit tests in `flv.rs` |
| Script tag | not byte-compared: ffmpeg writes 13 properties including `filesize`, ours writes 12 (no `filesize`, `encoder: "prostor"`); both decode with our AMF0 reader |

### RTMP publish to ffmpeg's RTMP server (localhost, `--realtime`)

| Measure | Value |
| --- | --- |
| TCP connect → handshake done | 1.3 ms |
| Handshake → `connect` accepted | 1.4 ms |
| `publish` accepted (ready to send media) | **1.7 ms** after connect |
| Media sent | 466 tags (182 video, 284 audio), 1,614,063 bytes, paced at 2.06 Mbit/s over 6.27 s |
| Received file (`ffprobe`) | h264 Main 1280x720 30 fps, aac LC 48 kHz stereo, 180 video + 283 audio packets — same as the input |
| Received audio/video tags identical to the sent ones (type, timestamp, body) | **466 of 466** |
| Server messages we do not handle | `SetPeerBandwidth` (type 6) and `onBWDone` surfaced as `Event::Unhandled`; harmless |
| Window acknowledgement | ffmpeg acked at 1,259,581 bytes; `rml_rtmp` tracks the peer window and sends our acks |

ffmpeg logs `Error during demuxing: Input/output error` when we close the socket after
`deleteStream`; the output file is finalized correctly anyway. Twitch and YouTube were not
attempted (no key available to the agent).

### `rml_rtmp` 0.8.0 evaluation

| Topic | Finding |
| --- | --- |
| Maintenance | 0.8.0 released 2023-04-29, nothing since (3.5 years); 300 k downloads; MIT |
| Dependencies | `bytes`, `byteorder`, `rml_amf0`, `thiserror 1`, `rand 0.8`, `sha2 0.9`, `hmac 0.10` — old majors, so they duplicate newer `sha2`/`rand` in the tree |
| Sans-IO fit | Good: `Handshake::process_bytes` and `ClientSession::handle_input` take bytes and return packets + events; `publish_video_data` / `publish_audio_data` take FLV tag bodies as-is with a `can_be_dropped` flag |
| Not pure | `ClientSession` reads `SystemTime::now()` for message timestamps, so it is native-only (gated with `cfg(not(target_arch = "wasm32"))`) |
| Handshake | FP9 digest handshake, command byte 3 (no encryption), version bytes 128.0.7.2; accepts the plain handshake from servers too |
| `connect` | sends `app`, `flashVer`, `objectEncoding 0` and `tcUrl` (we set it; some platforms require it) |
| Metadata | `publish_metadata` sends `@setDataFrame onMetaData` with an AMF0 **object** (OBS sends an ECMA array); ffmpeg accepted it; to be confirmed on Twitch/YouTube |
| Gaps | no RTMPS (wrap the socket with `tokio-rustls`), no reconnect logic (ours), no enhanced RTMP (not needed), `SetPeerBandwidth` unhandled |
| Alternative: write our own | handshake (~150 lines with digest), chunk serializer with header compression (~250), AMF0 commands `connect`/`createStream`/`publish`/`deleteStream` (~150), ack window and `SetChunkSize` (~80): roughly 600–700 lines plus tests |

## Conclusions for Phase 0

- **Adopt `rml_rtmp` wrapped**: `prostor_protocol::rtmp::Publisher` is the only thing the
  server sees (`new` → `handle_input` → `publish_metadata` / `publish_tag` → `stop`); swapping
  the implementation later does not touch the egress supervisor. Budget a rewrite only if
  a platform rejects its handshake or metadata, which the pending key test will tell.
- **Muxer API shape**: browser side `FlvMuxer` in `prostor-wasm` (`header`, `metadata`,
  `video_sequence_header`, `video`, `audio_sequence_header`, `audio`), each returning one
  tag's bytes; server side `flv::parse_tag` on each WebSocket message (borrowing, zero copy)
  or `flv::Demuxer` for files. RTMP messages and FLV tag bodies share the same layout, so
  the server forwards bodies untouched.
- **Pass `decoderConfig.description` verbatim.** ffmpeg's AudioSpecificConfig is 5 bytes
  (`11 90 56 e5 00`, with the SBR sync extension), not the textbook 2; the muxer must not
  assume a length.
- **Egress supervisor still needs**: TCP + TLS driver, reconnect with backoff, a bounded
  queue that drops `droppable` packets first, per-destination status events, and handling
  of `SetPeerBandwidth`.

## Open questions

- Twitch and YouTube acceptance of the FP9 handshake, `flashVer "FMLE/3.0 (compatible; prostor/…)"`
  and the object-shaped `onMetaData`: run step 4 above with a key.
- Reconnect behaviour per platform (YouTube and Twitch differ): needs the key as well.
- RTMPS for YouTube (`rtmps://a.rtmps.youtube.com:443/live2`): `tokio-rustls` wrapper, Phase 0 task.
