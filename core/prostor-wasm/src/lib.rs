//! Browser facade over the prostor core.
//!
//! This is the only crate that may depend on `wasm-bindgen`, `js-sys` or `web-sys`.
//! It exposes the engine and the protocol crate to TypeScript as plain data (serde
//! via `serde-wasm-bindgen`, shaped to match the `ts-rs` generated types) and byte
//! buffers. It holds no browser-specific policy of its own.

#![forbid(unsafe_code)]

use js_sys::{ArrayBuffer, Reflect, Uint8Array};
use prostor_model::{Layer, LayerId, LayerKind, Point, Rect, Scene, SceneId};
use prostor_protocol::flv;
use serde::Serialize;
use std::fmt;
use wasm_bindgen::{JsCast, prelude::*};

#[wasm_bindgen]
extern "C" {
    /// An `ArrayBuffer` or any typed array / `DataView`: what WebCodecs hands out as
    /// `decoderConfig.description`.
    #[wasm_bindgen(typescript_type = "BufferSource")]
    pub type BufferSource;
}

/// Copies a `BufferSource` into a `Vec`. wasm-bindgen's own `&[u8]` conversion reads
/// `.length`, which an `ArrayBuffer` lacks, so a description passed as a bare buffer
/// used to arrive as zero bytes; this handles every shape and refuses empty input.
fn buffer_source_bytes(value: &BufferSource, what: &str) -> Result<Vec<u8>, JsError> {
    let value: &JsValue = value.as_ref();
    let bytes = if let Some(array) = value.dyn_ref::<Uint8Array>() {
        array.to_vec()
    } else if let Some(buffer) = value.dyn_ref::<ArrayBuffer>() {
        Uint8Array::new(buffer).to_vec()
    } else if ArrayBuffer::is_view(value) {
        let field = |name: &str| {
            Reflect::get(value, &JsValue::from_str(name))
                .map_err(|_| JsError::new(&format!("{what}: cannot read {name}")))
        };
        let buffer = field("buffer")?;
        let offset = field("byteOffset")?.as_f64().unwrap_or(0.0) as u32;
        let length = field("byteLength")?.as_f64().unwrap_or(0.0) as u32;
        Uint8Array::new_with_byte_offset_and_length(&buffer, offset, length).to_vec()
    } else {
        return Err(JsError::new(&format!(
            "{what} must be an ArrayBuffer or a typed array"
        )));
    };
    if bytes.is_empty() {
        return Err(JsError::new(&format!("{what} is empty")));
    }
    Ok(bytes)
}

/// Runs once when the module is instantiated (`init()` on the JS side).
#[wasm_bindgen(start)]
fn start() {
    #[cfg(feature = "console_error_panic_hook")]
    console_error_panic_hook::set_once();
}

/// Version of the wasm facade.
#[wasm_bindgen]
pub fn version() -> String {
    env!("CARGO_PKG_VERSION").to_owned()
}

/// Hello-world round trip: a sample [`Scene`] built in Rust, returned as a plain
/// JS object matching the generated `Scene` TypeScript type.
#[wasm_bindgen]
pub fn hello_scene() -> Result<JsValue, JsValue> {
    to_js(&sample_scene())
}

/// Computes the compositor's drawing instructions for `scene` (a plain object matching the
/// generated `Scene` type) on a `width × height` program. Returns a `RenderPlan` object.
#[wasm_bindgen]
pub fn render_plan(scene: JsValue, width: u32, height: u32) -> Result<JsValue, JsValue> {
    let scene: Scene = serde_wasm_bindgen::from_value(scene)?;
    to_js(&prostor_engine::render_plan(&scene, (width, height)))
}

/// Serializes a value into a JSON-compatible JS object (maps as objects,
/// `None` as `null`), which is the shape the `ts-rs` types describe.
fn to_js<T: Serialize>(value: &T) -> Result<JsValue, JsValue> {
    let serializer = serde_wasm_bindgen::Serializer::json_compatible();
    value.serialize(&serializer).map_err(JsValue::from)
}

/// The scene returned by [`hello_scene`]; also used by the tests.
pub fn sample_scene() -> Scene {
    Scene {
        id: SceneId::new("scene-hello"),
        name: "Hello from Rust".into(),
        position: Point { x: 120.0, y: 80.0 },
        layers: vec![
            Layer {
                id: LayerId::new("layer-camera"),
                name: "Camera".into(),
                kind: LayerKind::Camera,
                rect: Rect::FULL,
                z: 0,
                crop: None,
            },
            Layer {
                id: LayerId::new("layer-title"),
                name: "Title".into(),
                kind: LayerKind::Text {
                    text: "prostor".into(),
                },
                rect: Rect {
                    x: 96.0,
                    y: 880.0,
                    w: 1200.0,
                    h: 120.0,
                },
                z: 1,
                crop: None,
            },
        ],
    }
}

/// Why a timestamp coming from JavaScript was refused.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TimestampError {
    /// `NaN` or infinite.
    NotFinite,
    /// Earlier than the origin the stream started at.
    BeforeOrigin,
    /// More than 2^32 - 1 milliseconds after the origin (the FLV field is 32-bit).
    OutOfRange,
}

impl fmt::Display for TimestampError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            TimestampError::NotFinite => write!(f, "timestamp is not a finite number"),
            TimestampError::BeforeOrigin => write!(f, "timestamp is before the stream origin"),
            TimestampError::OutOfRange => write!(f, "timestamp does not fit in 32 bits"),
        }
    }
}

impl std::error::Error for TimestampError {}

/// Largest value the FLV timestamp field holds, plus one.
const TIMESTAMP_LIMIT_MS: f64 = 4_294_967_296.0;

/// Timestamps up to this much before the origin map to 0 instead of failing. Both
/// tracks share one origin (the session start), and the first audio chunks land
/// slightly before it once the AAC encoder's priming delay has been subtracted from
/// their timestamps; anything earlier than this window is a real clock error.
pub const EARLY_WINDOW_MS: f64 = 200.0;

/// Turns the encoder's millisecond timestamps (an `f64` from JavaScript, possibly
/// fractional) into FLV timestamps relative to a stream origin. The first timestamp seen
/// becomes 0 unless [`TimestampMapper::set_origin`] was called; [`TimestampMapper::rebase`]
/// starts a new origin, for a new connection that must begin at 0 again. Timestamps up
/// to [`EARLY_WINDOW_MS`] before the origin clamp to 0 (see there); earlier ones fail.
///
/// Plain Rust so the policy is tested natively; the wasm facade only forwards to it.
#[derive(Debug, Default, Clone, PartialEq)]
pub struct TimestampMapper {
    origin: Option<f64>,
}

impl TimestampMapper {
    /// A mapper whose origin is the first timestamp it sees.
    pub fn new() -> Self {
        Self::default()
    }

    /// Pins the origin explicitly (for example the first media chunk's timestamp, so
    /// that sequence headers written earlier share it).
    pub fn set_origin(&mut self, origin_ms: f64) -> Result<(), TimestampError> {
        if !origin_ms.is_finite() {
            return Err(TimestampError::NotFinite);
        }
        self.origin = Some(origin_ms);
        Ok(())
    }

    /// The origin, once known.
    pub fn origin(&self) -> Option<f64> {
        self.origin
    }

    /// Forgets the origin; the next timestamp becomes 0.
    pub fn rebase(&mut self) {
        self.origin = None;
    }

    /// Maps a timestamp to FLV milliseconds, rounded to the nearest integer.
    pub fn map(&mut self, timestamp_ms: f64) -> Result<u32, TimestampError> {
        if !timestamp_ms.is_finite() {
            return Err(TimestampError::NotFinite);
        }
        let origin = *self.origin.get_or_insert(timestamp_ms);
        let relative = (timestamp_ms - origin).round();
        if relative < 0.0 {
            return if relative >= -EARLY_WINDOW_MS {
                Ok(0)
            } else {
                Err(TimestampError::BeforeOrigin)
            };
        }
        if relative >= TIMESTAMP_LIMIT_MS {
            return Err(TimestampError::OutOfRange);
        }
        Ok(relative as u32)
    }
}

/// Converts a JavaScript composition time (milliseconds, possibly fractional) to the
/// muxer's integer; the muxer range-checks it.
fn composition_time(ms: f64) -> Result<i32, TimestampError> {
    if !ms.is_finite() {
        return Err(TimestampError::NotFinite);
    }
    let rounded = ms.round();
    if rounded < f64::from(i32::MIN) || rounded > f64::from(i32::MAX) {
        return Err(TimestampError::OutOfRange);
    }
    Ok(rounded as i32)
}

/// FLV muxer for the browser encoder pipeline. Every method returns the bytes of the
/// tag it produced (a fresh `Uint8Array`), ready to be sent over the ingest WebSocket
/// or appended to a recording.
///
/// Timestamps are JavaScript numbers in milliseconds (`chunk.timestamp / 1000`); they
/// are rebased to the first one seen (or to [`FlvMuxer::set_origin`]) and must be finite,
/// not before the origin, and less than 2^32 ms after it. Byte-for-byte the same output
/// as the native muxer in `prostor-protocol`.
#[wasm_bindgen]
pub struct FlvMuxer {
    inner: flv::Muxer,
    clock: TimestampMapper,
    buf: Vec<u8>,
}

#[wasm_bindgen]
impl FlvMuxer {
    /// Creates a muxer for a stream with the given tracks.
    #[wasm_bindgen(constructor)]
    pub fn new(video: bool, audio: bool) -> FlvMuxer {
        FlvMuxer {
            inner: flv::Muxer::new(flv::MuxConfig { video, audio }),
            clock: TimestampMapper::new(),
            buf: Vec::with_capacity(512 * 1024),
        }
    }

    /// Pins the timestamp origin (milliseconds) instead of using the first tag's.
    pub fn set_origin(&mut self, origin_ms: f64) -> Result<(), JsError> {
        self.clock.set_origin(origin_ms)?;
        Ok(())
    }

    /// The timestamp origin, once known.
    pub fn origin(&self) -> Option<f64> {
        self.clock.origin()
    }

    /// Starts a new origin: the next tag's timestamp becomes 0.
    pub fn rebase(&mut self) {
        self.clock.rebase();
    }

    /// The FLV file header (send once, first).
    pub fn header(&mut self) -> Uint8Array {
        self.buf.clear();
        self.inner.write_header(&mut self.buf);
        self.finish()
    }

    /// The `onMetaData` script tag.
    #[allow(clippy::too_many_arguments)]
    pub fn metadata(
        &mut self,
        width: u32,
        height: u32,
        framerate: f64,
        video_kbps: f64,
        audio_kbps: f64,
        audio_sample_rate: u32,
        stereo: bool,
    ) -> Result<Uint8Array, JsError> {
        let metadata = flv::Metadata {
            width,
            height,
            framerate,
            video_kbps,
            audio_kbps,
            audio_sample_rate,
            stereo,
            encoder: format!("prostor/{}", env!("CARGO_PKG_VERSION")),
        };
        self.buf.clear();
        self.inner.write_metadata(&metadata, &mut self.buf)?;
        Ok(self.finish())
    }

    /// Video sequence header: `decoderConfig.description` of the WebCodecs H.264 encoder
    /// (the AVCDecoderConfigurationRecord), as the `ArrayBuffer` or typed array WebCodecs
    /// delivered it. Empty input is an error.
    pub fn video_sequence_header(
        &mut self,
        timestamp_ms: f64,
        avc_config: &BufferSource,
    ) -> Result<Uint8Array, JsError> {
        let avc_config = buffer_source_bytes(avc_config, "AVC decoder configuration")?;
        let ts = self.clock.map(timestamp_ms)?;
        self.buf.clear();
        self.inner
            .write_video_sequence_header(ts, &avc_config, &mut self.buf)?;
        Ok(self.finish())
    }

    /// One encoded video chunk (`EncodedVideoChunk` bytes in AVCC format).
    pub fn video(
        &mut self,
        timestamp_ms: f64,
        keyframe: bool,
        composition_time_ms: f64,
        data: &[u8],
    ) -> Result<Uint8Array, JsError> {
        let ts = self.clock.map(timestamp_ms)?;
        let cts = composition_time(composition_time_ms)?;
        self.buf.clear();
        self.inner
            .write_video(ts, keyframe, cts, data, &mut self.buf)?;
        Ok(self.finish())
    }

    /// Audio sequence header: `decoderConfig.description` of the WebCodecs AAC encoder
    /// (the AudioSpecificConfig), as the `ArrayBuffer` or typed array WebCodecs delivered
    /// it. Empty input is an error.
    pub fn audio_sequence_header(
        &mut self,
        timestamp_ms: f64,
        audio_specific_config: &BufferSource,
    ) -> Result<Uint8Array, JsError> {
        let config = buffer_source_bytes(audio_specific_config, "AudioSpecificConfig")?;
        let ts = self.clock.map(timestamp_ms)?;
        self.buf.clear();
        self.inner
            .write_audio_sequence_header(ts, &config, &mut self.buf)?;
        Ok(self.finish())
    }

    /// One raw AAC frame (`EncodedAudioChunk` bytes, no ADTS header).
    pub fn audio(&mut self, timestamp_ms: f64, data: &[u8]) -> Result<Uint8Array, JsError> {
        let ts = self.clock.map(timestamp_ms)?;
        self.buf.clear();
        self.inner.write_audio(ts, data, &mut self.buf)?;
        Ok(self.finish())
    }

    /// Tags produced so far.
    pub fn tags_written(&self) -> f64 {
        self.inner.tags_written() as f64
    }

    /// Bytes produced so far, header included.
    pub fn bytes_written(&self) -> f64 {
        self.inner.bytes_written() as f64
    }

    /// Copies the scratch buffer into a fresh JS array (one copy; the caller owns it).
    fn finish(&self) -> Uint8Array {
        Uint8Array::from(&self.buf[..])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mapper_rebases_to_the_first_timestamp_and_rounds() {
        let mut clock = TimestampMapper::new();
        assert_eq!(clock.origin(), None);
        assert_eq!(clock.map(1_000_000.4), Ok(0));
        assert_eq!(clock.origin(), Some(1_000_000.4));
        assert_eq!(clock.map(1_000_033.7), Ok(33));
        assert_eq!(clock.map(1_000_033.9), Ok(34), "rounded, not truncated");
        assert_eq!(
            clock.map(1_000_000.0),
            Ok(0),
            "within rounding of the origin"
        );
        assert_eq!(
            clock.map(999_999.0),
            Ok(0),
            "slightly early (priming-compensated audio) clamps to 0"
        );
        assert_eq!(clock.map(1_000_000.0 - EARLY_WINDOW_MS), Ok(0));
        assert_eq!(
            clock.map(1_000_000.0 - EARLY_WINDOW_MS - 1.0),
            Err(TimestampError::BeforeOrigin)
        );
    }

    #[test]
    fn mapper_rejects_non_finite_and_out_of_range_values() {
        let mut clock = TimestampMapper::new();
        assert_eq!(clock.map(f64::NAN), Err(TimestampError::NotFinite));
        assert_eq!(clock.map(f64::INFINITY), Err(TimestampError::NotFinite));
        assert_eq!(
            clock.origin(),
            None,
            "rejected values do not set the origin"
        );
        assert_eq!(clock.map(0.0), Ok(0));
        assert_eq!(clock.map(4_294_967_295.0), Ok(u32::MAX));
        assert_eq!(clock.map(4_294_967_295.4), Ok(u32::MAX));
        assert_eq!(clock.map(4_294_967_295.6), Err(TimestampError::OutOfRange));
        assert_eq!(clock.map(4_294_967_301.0), Err(TimestampError::OutOfRange));
        assert_eq!(clock.map(-1.0), Ok(0), "inside the early window");
        assert_eq!(clock.map(-1000.0), Err(TimestampError::BeforeOrigin));
    }

    #[test]
    fn mapper_honours_an_explicit_origin_and_rebase() {
        let mut clock = TimestampMapper::new();
        clock.set_origin(500.0).unwrap();
        assert_eq!(clock.map(512.0), Ok(12));
        assert_eq!(clock.map(499.0), Ok(0));
        assert_eq!(clock.map(200.0), Err(TimestampError::BeforeOrigin));
        assert_eq!(clock.set_origin(f64::NAN), Err(TimestampError::NotFinite));
        clock.rebase();
        assert_eq!(clock.origin(), None);
        assert_eq!(clock.map(80_000.0), Ok(0), "a new connection starts at 0");
        assert_eq!(clock.map(80_040.0), Ok(40));
    }

    #[test]
    fn composition_time_is_rounded_and_bounded() {
        assert_eq!(composition_time(0.0), Ok(0));
        assert_eq!(composition_time(66.6), Ok(67));
        assert_eq!(composition_time(-33.4), Ok(-33));
        assert_eq!(composition_time(f64::NAN), Err(TimestampError::NotFinite));
        assert_eq!(composition_time(1e12), Err(TimestampError::OutOfRange));
    }

    #[test]
    fn sample_scene_has_two_layers() {
        assert_eq!(sample_scene().layers.len(), 2);
    }
}
