//! Wire formats shared by the browser and the server (sans-IO).
//!
//! - [`flv`]: FLV container writer (muxer) and parser. Runs in the producer's browser
//!   as WASM and natively on the server (ingest parser, recording). The output is
//!   byte-identical in both runtimes; a fixture test proves it. The parser is written
//!   for untrusted input: every length is checked before it is trusted and the
//!   incremental reader is bounded.
//! - [`amf0`]: the AMF0 subset needed for `onMetaData` and RTMP command objects, with
//!   depth and size limits.
//! - [`ingest`]: framing of the ingest WebSocket's binary messages (sequence number plus
//!   one tag), used by the browser to send and by the server to parse.
//! - [`secret`]: a string type for stream keys and the producer secret that never
//!   prints its value.
//! - [`rtmp`] (native only): an RTMP publishing session on top of `rml_rtmp`. The caller
//!   owns the socket; the session only turns bytes into bytes and events.
//!
//! `flv`, `amf0` and `secret` perform no I/O and read no clock: inputs are byte slices
//! and timestamps, outputs are byte buffers. `rtmp` is the documented exception to
//! ADR 0001: `rml_rtmp` reads the system clock and the RNG internally, which makes the
//! module native-only and its output non-deterministic (see ADR 0004).
//!
//! Tests run with `cargo test` and with
//! `wasm-pack test --headless --chrome core/prostor-protocol`.

#![forbid(unsafe_code)]

pub mod amf0;
pub mod flv;
pub mod ingest;
#[cfg(not(target_arch = "wasm32"))]
pub mod rtmp;
pub mod secret;

/// Version of this crate, as compiled into the binary or the wasm module.
pub fn crate_version() -> &'static str {
    env!("CARGO_PKG_VERSION")
}
