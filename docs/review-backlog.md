# Review backlog

Open findings from the fresh-context reviews of 2026-10-14 (second pass, on the tree after
the first-pass fixes). Line numbers are approximate. Close items by fixing them in the
milestone that touches the code, with a regression test each.

## Closed in M1

- 1 (idle timeout rebuilt per loop turn): `Peer` keeps one deadline, pushed only by
  received frames; tests cover a dead peer, a pong-answering peer and `MAX_SOCKETS`.
- 2 (`FlvMuxer` copies an `ArrayBuffer` as zero bytes): the sequence-header methods take
  a `BufferSource` (typed array, `ArrayBuffer` or `DataView`, offsets honoured) and refuse
  empty input; browser test covers every shape.
- 3 (`TimestampMapper` throws on early audio): timestamps up to 200 ms before the origin
  map to 0 (`EARLY_WINDOW_MS`); the Worker pins the origin at session start and subtracts
  the AAC priming delay from audio.
- 9 (`IngestClientMessage` `Debug` prints `auth`): hand-written `Debug` with `<redacted>`.
- 10 (`EgressState` u64 fields export as `bigint`): `#[ts(type = "number")]`.
- 11 (`Close` dropped before the reply is flushed): `Peer` completes every close
  handshake, bounded by 2 s; tests assert the code the client sees.

## Warnings
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

- Graceful shutdown covering WebSockets: done in M1 (`shutdown.rs` wired into `main` and
  both handlers; close 1001, recordings finalized, `DRAIN_TIMEOUT`, exit on the second
  signal).
- Secret enforcement: done for the ingest socket in M1 (`hello.auth`). `Origin`
  allow-list on both upgrades and auth on the signaling socket remain for M2.
- Connect-time refusal of loopback/link-local/private ranges and connect/handshake
  timeouts in the RTMP egress (M2).
- A `cargo-fuzz` job for `parse_tag`, `Demuxer`, `amf0::read_value`, `Header::parse`.
