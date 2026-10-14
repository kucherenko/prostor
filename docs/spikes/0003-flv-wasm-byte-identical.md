# Spike (c): the FLV muxer compiled to WASM is byte-identical and cheap

**Outcome:** the same fixture re-muxed natively (`cargo test`) and in headless Chrome 155
(`wasm-pack test`) hashes to the same SHA-256, `be46d2b3…56fe3`. One `write_video` call with
a 25 KB payload costs 0.41 µs in release WASM, the same as native; crossing the JS → WASM
boundary with copies costs 9.7 µs per video tag and 0.9 µs per audio tag, which is 0.03 %
of one core at 30 fps + 47 AAC frames/s. The muxer can live in WASM without any batching.

- Date: 2026-10-14. Chrome 155.0.8059.40 (headless), wasm-pack 0.15.0, wasm-bindgen 0.2.129,
  Apple M1 Pro. Load average during timings: 10–12 (other spikes were building), so treat
  the absolute numbers as upper bounds.
- Code: `core/prostor-protocol/tests/flv_both_runtimes.rs` (runs in both runtimes),
  `core/prostor-wasm/src/lib.rs` (`FlvMuxer`), `spikes/rtmp-publish/wasm-overhead.mjs`.

## Quick path (reproduce)

```sh
cargo test -p prostor-protocol --test flv_both_runtimes -- --nocapture          # native
just test-wasm                                                                 # both wasm crates in Chrome
wasm-pack test --headless --chrome --release core/prostor-protocol --test flv_both_runtimes -- --nocapture
just build-wasm && node spikes/rtmp-publish/wasm-overhead.mjs                  # JS -> WASM boundary
```

## Results

### Byte identity

| Runtime | Command | SHA-256 of the re-muxed fixture (83 tags, our header + metadata + 82 media tags) |
| --- | --- | --- |
| native (aarch64) | `cargo test -p prostor-protocol` | `be46d2b376ba2034ad43a35c7f9bd82fc7600d5cf6b18e2733771bdee7d56fe3` |
| wasm32 in Chrome 155 | `wasm-pack test --headless --chrome core/prostor-protocol` | `be46d2b376ba2034ad43a35c7f9bd82fc7600d5cf6b18e2733771bdee7d56fe3` |

The hash is a committed constant in the test, so CI fails if either runtime drifts.

### Cost per `Muxer::write_video` (25 KB payload, 10 000 calls, buffer reused)

| Build | µs per call |
| --- | --- |
| native, debug | 0.49 |
| native, release | 0.41 |
| wasm, debug (`wasm-pack test` default) | 48.5 |
| wasm, release (`wasm-pack test --release`) | **0.41** |

Debug WASM is two orders of magnitude slower than release WASM; never read performance
numbers from a default `wasm-pack test` run.

### JS → WASM boundary (`FlvMuxer` from the `wasm-release` package, headless Chrome)

| Call | µs per call | Note |
| --- | --- | --- |
| `FlvMuxer.video(ts, key, cts, Uint8Array(25 KB))` | 9.72 | copy in, mux, copy out to a new `Uint8Array` |
| `FlvMuxer.audio(ts, Uint8Array(400 B))` | 0.91 | |
| baseline `Uint8Array.slice()` 25 KB in JS | 3.39 | one copy, no WASM |
| baseline `Uint8Array.slice()` 400 B in JS | 0.54 | |
| implied per second at 30 fps video + 47 AAC frames | 334 µs | 0.033 % of one core |

## Conclusions for Phase 0

- Keep the muxer in Rust/WASM; the boundary cost is noise next to encoding. One call per
  encoded chunk is fine; no batching or shared-memory tricks needed.
- The returned `Uint8Array` is a fresh copy: hand it straight to `WebSocket.send()` or a
  recording buffer; no further copies are needed on the JS side.
- Build the production package with `--profile wasm-release` (already the `just build-wasm`
  default); the `FlvMuxer` added 12 KB to the package (32 KB → 44 KB).
- The parser (`parse_tag`, `Demuxer`) runs in the same tests in both runtimes, so the server
  ingest parser and the browser muxer are verified against each other continuously.

## Open questions

- Transferring the `Uint8Array` out of the compositor Worker to the WebSocket on the main
  thread (if the socket does not live in the worker) adds a `postMessage` transfer; measure
  when the ingest adapter exists.
