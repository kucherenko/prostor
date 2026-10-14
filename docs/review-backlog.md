# Review backlog

Open findings from the fresh-context reviews of 2026-10-14 (second pass, on the tree after
the first-pass fixes). Line numbers are approximate. Close items by fixing them in the
milestone that touches the code, with a regression test each.

## Warnings

1. `server/prostor-server/src/ws.rs` `Peer::next`: the idle timeout is rebuilt on every loop
   turn and the 10 s ping tick wins the select, so silent sockets never time out and 64 of
   them exhaust `MAX_SOCKETS` (503 for everyone). One deadline per `next()`, reset only by
   received frames; tests for `IDLE_TIMEOUT` and `MAX_SOCKETS`. (M1 ingest handler.)
2. `core/prostor-wasm/src/lib.rs` `FlvMuxer`: methods take `&[u8]`; an `ArrayBuffer` (what
   WebCodecs gives for `decoderConfig.description`) is copied as zero bytes without error,
   producing an undecodable stream. Accept `Uint8Array`/BufferSource and reject empty
   sequence-header configs. (M1 encoder adapter.)
3. `core/prostor-wasm/src/lib.rs` `TimestampMapper`: one origin shared by both tracks,
   latched by the first tag; earlier audio timestamps (AAC priming compensation) throw
   `BeforeOrigin`. Require `set_origin` up front or accept a small negative window; document.
   (M1.)
4. `core/prostor-protocol/src/rtmp.rs` `State::Stopped` is not terminal: late replies revive
   the session and resend the key. Ignore input and emit nothing once stopped. (M2.)
5. `core/prostor-protocol/src/rtmp.rs`: `rml_rtmp`'s own decoders on bytes from a
   user-chosen destination can abort (10 000 nested AMF0 arrays on a 2 MiB stack),
   panic on chunk-size/csid sequences ("capacity overflow") or on a command with fewer than
   three values. Isolate each Publisher (dedicated thread, `catch_unwind`, bigger stack) and
   pre-check inbound message sizes, or take ADR 0004's rewrite path. (M2.)
6. `rtmp.rs` `parse_parts` cuts at the first `?`, dropping query parameters that belong to
   the key or app (Twitch `?bandwidthtest=true`, YouTube `?backup=1`). `Target::new` and
   `Target::parse` disagree; the unit test asserts the stripping. (M2.)
7. `rtmp.rs` rejection mapping is a six-code allow-list that drops `level`; an error-level
   `NetStream.Publish.*` other than `Start` (e.g. `Unauthorized`) leaves the session in
   `RequestingStream` forever. Supervisor timeout in that state; treat any non-Start
   publish status as a rejection. (M2.)
8. `rtmp.rs` `Error` derives `Debug` and wraps `ClientSessionError`; a non-UTF-8 AMF0 string
   from the server prints raw server bytes (possibly the key). Hand-written `Debug` with
   variant names only. (M2.)
9. `core/prostor-model/src/ingest.rs` `IngestClientMessage` derives `Debug` with
   `auth: Option<String>`, so `{:?}` prints the producer secret. Redacting wrapper. (M1.)
10. `ingest.rs` `EgressState`: u64 fields export as `bigint` in ts-rs while serde_json sends
    numbers. `#[ts(type = "number")]`. (M1.)
11. `ws.rs` `Message::Close(_) => break` drops the socket before tungstenite flushes the
    Close reply; clients see 1006. Keep reading until `None` or close explicitly. Likely the
    real cause of the 1006 in `docs/spikes/0004`. (M1.)
12. `core/prostor-engine/src/layout.rs` `render_plan` clamps to the program instead of
    scaling from the 1920×1080 authoring space and ignores `crop`; on 1280×720 the sample
    title is invisible. (M3, or M1 if the program size becomes configurable.)

## Suggestions

13. `web/src/lib/wasm/index.ts` memoises a rejected `init()` promise; reset on failure.
14. `.github/workflows/ci.yml` "types up to date" uses `git diff --exit-code`, which ignores
    untracked files; use `git add -N` or `git status --porcelain`.
15. In-crate `#[test]`s in `prostor-protocol` never run under `wasm-pack test`, so the
    32-bit-sensitive guards are native-only; use `#[wasm_bindgen_test(unsupported = test)]`
    or move them to `tests/`.

## Verified but not detailed (see the reviewer's notes)

- `flv.rs` `Demuxer::feed` rejects valid large input; `parse_script_tag` rejects a real
  `onMetaData` with a keyframe index; `TimestampUnwrapper` double wrap; `drain_tags` loses
  tags on error.
- `rtmp.rs` `Target::new` given a full URL; `publish_metadata` ignores `MuxConfig`.
- `ws.rs` wrong close codes; a slow reader wedges the handler.
- `app.rs` `is_within` is only a lexical check.

## Deferred from the first pass

- Graceful shutdown covering WebSockets (in progress on `feat/m1-tracer-bullet`).
- Secret + Origin enforcement on the WebSocket upgrades (M2, same commit as real ingest).
- Connect-time refusal of loopback/link-local/private ranges and connect/handshake
  timeouts in the RTMP egress (M2).
- A `cargo-fuzz` job for `parse_tag`, `Demuxer`, `amf0::read_value`, `Header::parse`.
