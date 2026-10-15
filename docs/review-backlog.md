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

## Closed in M2

- 4 (`Stopped` not terminal): input is ignored and nothing is sent after `stop`.
- 5 (`rml_rtmp` decoders on hostile bytes): panics are caught per connection and turn into
  a reconnect; inbound bytes are capped at 1 MiB per connection. A stack overflow from
  deeply nested AMF0 is not catchable and remains open: the rewrite path of ADR 0004 is the
  real fix, a bigger worker stack the stopgap if a platform ever sends such data.
- 6 (query strings cut from the key): kept on the key by both `Target` constructors.
- 7 (non-`Start` publish statuses ignored): any status answering a pending connect or
  publish other than the accepting one is a rejection; connect/publish also have timeouts.
- 8 (`Error` prints server bytes): variant names only.
- Secret + `Origin` enforcement on both upgrades, and connect-time refusal of private
  ranges with connect/handshake timeouts: done.
- 9 (`IngestClientMessage` `Debug` prints `auth`): hand-written `Debug` with `<redacted>`.
- 10 (`EgressState` u64 fields export as `bigint`): `#[ts(type = "number")]`.
- 11 (`Close` dropped before the reply is flushed): `Peer` completes every close
  handshake, bounded by 2 s; tests assert the code the client sees.

## Warnings
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
- Secret enforcement: ingest in M1, signaling and `Origin` in M2.
- A `cargo-fuzz` job for `parse_tag`, `Demuxer`, `amf0::read_value`, `Header::parse`.
