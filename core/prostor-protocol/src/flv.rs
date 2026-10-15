//! FLV container: tag writer (muxer) and tag parser (demuxer), sans-IO.
//!
//! On-the-wire layout (Adobe "Video File Format Specification", version 10):
//!
//! ```text
//! header (9 bytes) | PreviousTagSize0 = 0 | tag | PreviousTagSize | tag | PreviousTagSize ...
//! tag = type (1) | body size (3) | timestamp (3) | timestamp extended (1) | stream id (3) | body
//! ```
//!
//! Timestamps are milliseconds. The muxer appends into caller-provided buffers and does not
//! allocate on the hot path. Only H.264 (AVC) video and AAC audio bodies are produced, which
//! is all RTMP destinations accept; the parser understands the same two codecs.
//!
//! The parser treats its input as hostile: lengths are validated before anything is
//! buffered or sliced, the incremental [`Demuxer`] has configurable limits, and its first
//! error is sticky.

use crate::amf0::{self, Value};
use bytes::{Bytes, BytesMut};
use std::fmt;

/// Size of the file header.
pub const HEADER_LEN: usize = 9;
/// Size of a tag header.
pub const TAG_HEADER_LEN: usize = 11;
/// Size of the `PreviousTagSize` trailer that follows every tag (and the file header).
pub const PREVIOUS_TAG_SIZE_LEN: usize = 4;
/// Largest tag body the 24-bit size field can describe.
pub const MAX_BODY_LEN: usize = 0xFF_FFFF;
/// Largest header data offset accepted. The spec reserves the field for future header
/// fields; every known writer uses 9. The cap keeps a hostile header from making the
/// parser wait for gigabytes.
pub const MAX_DATA_OFFSET: u32 = 64;
/// Largest script tag body [`parse_script_tag`] decodes (`onMetaData` is a few hundred
/// bytes).
pub const MAX_SCRIPT_BODY_LEN: usize = 64 * 1024;
/// Default per-tag body limit of the [`Demuxer`]: 4 MiB. A 1080p keyframe at 6 Mbit/s is
/// about 150 KB.
pub const DEFAULT_MAX_BODY_LEN: usize = 4 * 1024 * 1024;
/// Default limit on bytes the [`Demuxer`] buffers without finding a complete tag.
pub const DEFAULT_MAX_PENDING: usize = 2 * DEFAULT_MAX_BODY_LEN;
/// Largest composition time offset the signed 24-bit field can carry.
pub const MAX_COMPOSITION_TIME_MS: i32 = 0x7F_FFFF;
/// Smallest composition time offset the signed 24-bit field can carry.
pub const MIN_COMPOSITION_TIME_MS: i32 = -0x80_0000;

/// FLV `CodecID` for H.264.
pub const CODEC_ID_AVC: u8 = 7;
/// FLV `SoundFormat` for AAC.
pub const SOUND_FORMAT_AAC: u8 = 10;
/// Header byte of every AAC audio tag: AAC, "44 kHz", 16-bit, stereo. The real sample rate
/// and channel count live in the AudioSpecificConfig, so this byte is constant by spec.
pub const AAC_SOUND_HEADER: u8 = 0xAF;

/// Tag type.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum TagKind {
    Audio,
    Video,
    Script,
}

impl TagKind {
    /// The on-the-wire tag type.
    pub const fn as_u8(self) -> u8 {
        match self {
            TagKind::Audio => 8,
            TagKind::Video => 9,
            TagKind::Script => 18,
        }
    }

    /// Parses the on-the-wire tag type.
    pub const fn from_u8(value: u8) -> Option<Self> {
        match value {
            8 => Some(TagKind::Audio),
            9 => Some(TagKind::Video),
            18 => Some(TagKind::Script),
            _ => None,
        }
    }
}

/// What a tag means to a forwarding policy. Sequence headers and keyframes must never be
/// dropped; audio is never dropped either (independent frames, and gaps break A/V sync);
/// delta frames may be dropped, but then everything up to the next keyframe must go too.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum TagClass {
    /// AVC or AAC sequence header: cache it and resend it on every reconnect.
    SequenceHeader,
    /// A video keyframe.
    Key,
    /// A video inter frame (disposable or not).
    Delta,
    /// The AVC end-of-sequence marker.
    EndOfSequence,
    /// A raw AAC frame.
    Audio,
    /// A script tag (`onMetaData`).
    Script,
}

/// Error produced by the parser or the muxer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Error {
    /// The input does not start with `FLV`.
    BadSignature,
    /// Only version 1 exists.
    UnsupportedVersion(u8),
    /// The header's data offset is smaller than the header or larger than
    /// [`MAX_DATA_OFFSET`].
    BadDataOffset(u32),
    /// A tag type other than audio, video or script.
    UnknownTagType(u8),
    /// The tag's filter bit is set (encrypted FLV), which is not supported.
    EncryptedTag,
    /// The trailer after a tag does not match the tag's size.
    BadPreviousTagSize { expected: u32, found: u32 },
    /// A body larger than the 24-bit size field allows.
    BodyTooLarge(usize),
    /// A tag header announces a body larger than the parser's limit.
    BodyExceedsLimit { len: usize, limit: usize },
    /// The [`Demuxer`] was fed more bytes than its pending limit without a complete tag.
    PendingTooLarge { pending: usize, limit: usize },
    /// [`parse_exact`] was given less than one whole tag.
    Truncated,
    /// [`parse_exact`] was given more than one whole tag.
    TrailingBytes(usize),
    /// A video or audio body shorter than its codec header.
    TruncatedBody,
    /// A codec other than AVC video or AAC audio.
    UnsupportedCodec(u8),
    /// An enhanced-RTMP video tag (FourCC codecs), which is not supported.
    EnhancedRtmpVideo,
    /// A video frame type other than key, inter, disposable inter or generated key.
    BadFrameType(u8),
    /// An unknown AVC or AAC packet type.
    BadPacketType(u8),
    /// A tag of another kind was given where video or audio was expected.
    WrongKind(TagKind),
    /// A composition time outside the signed 24-bit range.
    CompositionTimeOutOfRange(i32),
    /// A script tag body larger than [`MAX_SCRIPT_BODY_LEN`].
    ScriptTooLarge(usize),
    /// The script tag body is not `name, value`.
    Amf0(amf0::Error),
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::BadSignature => write!(f, "not an FLV stream (bad signature)"),
            Error::UnsupportedVersion(v) => write!(f, "unsupported FLV version {v}"),
            Error::BadDataOffset(o) => write!(f, "invalid FLV header data offset {o}"),
            Error::UnknownTagType(t) => write!(f, "unknown FLV tag type {t}"),
            Error::EncryptedTag => write!(f, "encrypted FLV tags are not supported"),
            Error::BadPreviousTagSize { expected, found } => {
                write!(
                    f,
                    "PreviousTagSize {found} does not match tag size {expected}"
                )
            }
            Error::BodyTooLarge(n) => write!(f, "tag body of {n} bytes exceeds the 24-bit limit"),
            Error::BodyExceedsLimit { len, limit } => {
                write!(f, "tag body of {len} bytes exceeds the limit of {limit}")
            }
            Error::PendingTooLarge { pending, limit } => {
                write!(
                    f,
                    "{pending} buffered bytes without a complete tag (limit {limit})"
                )
            }
            Error::Truncated => write!(f, "message does not hold a complete tag"),
            Error::TrailingBytes(n) => write!(f, "{n} bytes follow the tag"),
            Error::TruncatedBody => write!(f, "tag body is shorter than its codec header"),
            Error::UnsupportedCodec(c) => write!(f, "unsupported codec id {c}"),
            Error::EnhancedRtmpVideo => write!(f, "enhanced RTMP video is not supported"),
            Error::BadFrameType(t) => write!(f, "unsupported video frame type {t}"),
            Error::BadPacketType(p) => write!(f, "unknown packet type {p}"),
            Error::WrongKind(k) => write!(f, "unexpected tag kind {k:?}"),
            Error::CompositionTimeOutOfRange(ms) => {
                write!(f, "composition time {ms} ms does not fit in 24 bits")
            }
            Error::ScriptTooLarge(n) => write!(f, "script tag of {n} bytes is too large"),
            Error::Amf0(e) => write!(f, "invalid script tag: {e}"),
        }
    }
}

impl std::error::Error for Error {}

impl From<amf0::Error> for Error {
    fn from(e: amf0::Error) -> Self {
        Error::Amf0(e)
    }
}

/// The file header.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Header {
    pub has_audio: bool,
    pub has_video: bool,
}

impl Header {
    /// Appends the 9-byte header followed by `PreviousTagSize0` (13 bytes in total).
    pub fn write(&self, out: &mut Vec<u8>) -> usize {
        out.extend_from_slice(b"FLV");
        out.push(1);
        out.push((u8::from(self.has_audio) << 2) | u8::from(self.has_video));
        out.extend_from_slice(&(HEADER_LEN as u32).to_be_bytes());
        out.extend_from_slice(&0u32.to_be_bytes());
        HEADER_LEN + PREVIOUS_TAG_SIZE_LEN
    }

    /// Parses the header and the `PreviousTagSize0` that follows it. Returns the header
    /// and the number of bytes consumed, or `Ok(None)` when more input is needed. A data
    /// offset outside `9..=MAX_DATA_OFFSET` is rejected as soon as the 9 header bytes are
    /// present.
    pub fn parse(input: &[u8]) -> Result<Option<(Header, usize)>, Error> {
        if input.len() < HEADER_LEN {
            return Ok(None);
        }
        if &input[..3] != b"FLV" {
            return Err(Error::BadSignature);
        }
        if input[3] != 1 {
            return Err(Error::UnsupportedVersion(input[3]));
        }
        let flags = input[4];
        let data_offset = u32::from_be_bytes([input[5], input[6], input[7], input[8]]);
        if !(HEADER_LEN as u32..=MAX_DATA_OFFSET).contains(&data_offset) {
            return Err(Error::BadDataOffset(data_offset));
        }
        // data_offset <= 64, so this cannot overflow on any target.
        let total = data_offset as usize + PREVIOUS_TAG_SIZE_LEN;
        if input.len() < total {
            return Ok(None);
        }
        Ok(Some((
            Header {
                has_audio: flags & 0b100 != 0,
                has_video: flags & 0b001 != 0,
            },
            total,
        )))
    }
}

/// A tag borrowed from a byte buffer: kind, timestamp and body (everything after the
/// 11-byte tag header, codec header byte included).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Tag<'a> {
    pub kind: TagKind,
    pub timestamp_ms: u32,
    pub body: &'a [u8],
}

impl<'a> Tag<'a> {
    /// Bytes this tag occupies on the wire, trailer included.
    pub fn wire_len(&self) -> usize {
        TAG_HEADER_LEN + self.body.len() + PREVIOUS_TAG_SIZE_LEN
    }

    /// Appends this tag (header, body and trailer) to `out`.
    pub fn write(&self, out: &mut Vec<u8>) -> Result<usize, Error> {
        write_tag(self.kind, self.timestamp_ms, self.body, out)
    }

    /// Copies the tag into an owned buffer.
    pub fn to_owned(&self) -> TagBuf {
        TagBuf {
            kind: self.kind,
            timestamp_ms: self.timestamp_ms,
            body: Bytes::copy_from_slice(self.body),
        }
    }

    /// Interprets the body as an AVC video tag.
    pub fn video(&self) -> Result<VideoTag<'a>, Error> {
        if self.kind != TagKind::Video {
            return Err(Error::WrongKind(self.kind));
        }
        VideoTag::parse(self.body)
    }

    /// Interprets the body as an AAC audio tag.
    pub fn audio(&self) -> Result<AudioTag<'a>, Error> {
        if self.kind != TagKind::Audio {
            return Err(Error::WrongKind(self.kind));
        }
        AudioTag::parse(self.body)
    }

    /// Classifies the tag for forwarding policies; fails on bodies the codec parsers
    /// reject, so malformed media never reaches a destination.
    pub fn class(&self) -> Result<TagClass, Error> {
        Ok(match self.kind {
            TagKind::Script => TagClass::Script,
            TagKind::Audio => match self.audio()?.packet_type {
                AacPacketType::SequenceHeader => TagClass::SequenceHeader,
                AacPacketType::Raw => TagClass::Audio,
            },
            TagKind::Video => {
                let video = self.video()?;
                match video.packet_type {
                    AvcPacketType::SequenceHeader => TagClass::SequenceHeader,
                    AvcPacketType::EndOfSequence => TagClass::EndOfSequence,
                    AvcPacketType::Nalu => match video.frame_type {
                        FrameType::Key | FrameType::GeneratedKey => TagClass::Key,
                        FrameType::Inter | FrameType::DisposableInter => TagClass::Delta,
                    },
                }
            }
        })
    }
}

/// An owned tag. The body is reference counted so fan-out to several consumers costs no
/// copies.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TagBuf {
    pub kind: TagKind,
    pub timestamp_ms: u32,
    pub body: Bytes,
}

impl TagBuf {
    /// Borrows the tag.
    pub fn as_tag(&self) -> Tag<'_> {
        Tag {
            kind: self.kind,
            timestamp_ms: self.timestamp_ms,
            body: &self.body,
        }
    }
}

/// Video frame type (upper nibble of the video header byte). The spec also defines
/// type 5 ("video info/command frame"); it carries no AVC packet and is rejected.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FrameType {
    Key,
    Inter,
    DisposableInter,
    GeneratedKey,
}

impl FrameType {
    const fn from_u8(value: u8) -> Option<Self> {
        match value {
            1 => Some(FrameType::Key),
            2 => Some(FrameType::Inter),
            3 => Some(FrameType::DisposableInter),
            4 => Some(FrameType::GeneratedKey),
            _ => None,
        }
    }

    const fn as_u8(self) -> u8 {
        match self {
            FrameType::Key => 1,
            FrameType::Inter => 2,
            FrameType::DisposableInter => 3,
            FrameType::GeneratedKey => 4,
        }
    }
}

/// `AVCPacketType`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AvcPacketType {
    /// Body carries the AVCDecoderConfigurationRecord.
    SequenceHeader,
    /// Body carries length-prefixed NAL units.
    Nalu,
    /// Empty body; marks the end of the stream.
    EndOfSequence,
}

impl AvcPacketType {
    const fn from_u8(value: u8) -> Option<Self> {
        match value {
            0 => Some(AvcPacketType::SequenceHeader),
            1 => Some(AvcPacketType::Nalu),
            2 => Some(AvcPacketType::EndOfSequence),
            _ => None,
        }
    }

    const fn as_u8(self) -> u8 {
        match self {
            AvcPacketType::SequenceHeader => 0,
            AvcPacketType::Nalu => 1,
            AvcPacketType::EndOfSequence => 2,
        }
    }
}

/// `AACPacketType`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AacPacketType {
    /// Body carries the AudioSpecificConfig.
    SequenceHeader,
    /// Body carries one raw AAC frame.
    Raw,
}

/// A parsed AVC video tag body.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct VideoTag<'a> {
    pub frame_type: FrameType,
    pub packet_type: AvcPacketType,
    /// Presentation minus decoding time, in milliseconds.
    pub composition_time_ms: i32,
    /// AVCDecoderConfigurationRecord, NAL units, or empty.
    pub data: &'a [u8],
}

impl<'a> VideoTag<'a> {
    /// Parses a video tag body.
    pub fn parse(body: &'a [u8]) -> Result<Self, Error> {
        let first = *body.first().ok_or(Error::TruncatedBody)?;
        if first & 0x80 != 0 {
            return Err(Error::EnhancedRtmpVideo);
        }
        let codec_id = first & 0x0F;
        if codec_id != CODEC_ID_AVC {
            return Err(Error::UnsupportedCodec(codec_id));
        }
        let frame_type = FrameType::from_u8(first >> 4).ok_or(Error::BadFrameType(first >> 4))?;
        if body.len() < 5 {
            return Err(Error::TruncatedBody);
        }
        let packet_type = AvcPacketType::from_u8(body[1]).ok_or(Error::BadPacketType(body[1]))?;
        Ok(VideoTag {
            frame_type,
            packet_type,
            composition_time_ms: composition_time_from_bytes([body[2], body[3], body[4]]),
            data: &body[5..],
        })
    }
}

/// A parsed AAC audio tag body.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AudioTag<'a> {
    pub packet_type: AacPacketType,
    /// AudioSpecificConfig or one raw AAC frame.
    pub data: &'a [u8],
}

impl<'a> AudioTag<'a> {
    /// Parses an audio tag body.
    pub fn parse(body: &'a [u8]) -> Result<Self, Error> {
        let first = *body.first().ok_or(Error::TruncatedBody)?;
        let sound_format = first >> 4;
        if sound_format != SOUND_FORMAT_AAC {
            return Err(Error::UnsupportedCodec(sound_format));
        }
        if body.len() < 2 {
            return Err(Error::TruncatedBody);
        }
        let packet_type = match body[1] {
            0 => AacPacketType::SequenceHeader,
            1 => AacPacketType::Raw,
            other => return Err(Error::BadPacketType(other)),
        };
        Ok(AudioTag {
            packet_type,
            data: &body[2..],
        })
    }
}

/// Parses one complete tag plus its `PreviousTagSize` trailer from the start of `input`.
/// Returns the tag and the number of bytes consumed, or `Ok(None)` when `input` does not
/// yet hold a complete tag. Feed arbitrary chunk boundaries; nothing is consumed until a
/// whole tag is present.
pub fn parse_tag(input: &[u8]) -> Result<Option<(Tag<'_>, usize)>, Error> {
    parse_tag_with_limit(input, MAX_BODY_LEN)
}

/// Like [`parse_tag`], but rejects any body larger than `max_body` as soon as the 11-byte
/// tag header is present, before anything is buffered.
pub fn parse_tag_with_limit(
    input: &[u8],
    max_body: usize,
) -> Result<Option<(Tag<'_>, usize)>, Error> {
    if input.len() < TAG_HEADER_LEN {
        return Ok(None);
    }
    if input[0] & 0b0010_0000 != 0 {
        return Err(Error::EncryptedTag);
    }
    let kind = TagKind::from_u8(input[0] & 0x1F).ok_or(Error::UnknownTagType(input[0]))?;
    // 24-bit field: at most 0xFF_FFFF, so the sums below cannot overflow on any target.
    let body_len = u32::from_be_bytes([0, input[1], input[2], input[3]]) as usize;
    if body_len > max_body {
        return Err(Error::BodyExceedsLimit {
            len: body_len,
            limit: max_body,
        });
    }
    let timestamp_ms = u32::from_be_bytes([input[7], input[4], input[5], input[6]]);
    let total = TAG_HEADER_LEN + body_len + PREVIOUS_TAG_SIZE_LEN;
    if input.len() < total {
        return Ok(None);
    }
    let expected = (TAG_HEADER_LEN + body_len) as u32;
    let found = u32::from_be_bytes(input[total - 4..total].try_into().expect("4 bytes"));
    if found != expected {
        return Err(Error::BadPreviousTagSize { expected, found });
    }
    Ok(Some((
        Tag {
            kind,
            timestamp_ms,
            body: &input[TAG_HEADER_LEN..TAG_HEADER_LEN + body_len],
        },
        total,
    )))
}

/// Parses a message that must hold exactly one tag (the ingest framing: one tag per
/// WebSocket message). Truncated or trailing bytes are errors, not silent acceptance.
pub fn parse_exact(message: &[u8]) -> Result<Tag<'_>, Error> {
    match parse_tag(message)? {
        None => Err(Error::Truncated),
        Some((_, n)) if n != message.len() => Err(Error::TrailingBytes(message.len() - n)),
        Some((tag, _)) => Ok(tag),
    }
}

/// Decodes a script tag body (`name, value`), refusing bodies larger than
/// [`MAX_SCRIPT_BODY_LEN`] and anything but a string name followed by one value.
pub fn parse_script_tag(body: &[u8]) -> Result<(String, Value), Error> {
    if body.len() > MAX_SCRIPT_BODY_LEN {
        return Err(Error::ScriptTooLarge(body.len()));
    }
    let (name, n) = amf0::read_value(body)?;
    let Value::String(name) = name else {
        return Err(Error::Amf0(amf0::Error::UnknownMarker(body[0])));
    };
    let (value, m) = amf0::read_value(&body[n..])?;
    if n + m != body.len() {
        return Err(Error::TrailingBytes(body.len() - n - m));
    }
    Ok((name, value))
}

/// Appends a complete tag (header, body, trailer) to `out`; returns the bytes appended.
pub fn write_tag(
    kind: TagKind,
    timestamp_ms: u32,
    body: &[u8],
    out: &mut Vec<u8>,
) -> Result<usize, Error> {
    write_tag_with(kind, timestamp_ms, out, |out| {
        out.extend_from_slice(body);
        Ok(())
    })
}

/// Appends a tag whose body is produced by `body`, then patches the size field. On error
/// nothing is appended.
fn write_tag_with(
    kind: TagKind,
    timestamp_ms: u32,
    out: &mut Vec<u8>,
    body: impl FnOnce(&mut Vec<u8>) -> Result<(), Error>,
) -> Result<usize, Error> {
    let start = out.len();
    let ts = timestamp_ms.to_be_bytes();
    out.extend_from_slice(&[kind.as_u8(), 0, 0, 0, ts[1], ts[2], ts[3], ts[0], 0, 0, 0]);
    if let Err(e) = body(out) {
        out.truncate(start);
        return Err(e);
    }
    let body_len = out.len() - start - TAG_HEADER_LEN;
    if body_len > MAX_BODY_LEN {
        out.truncate(start);
        return Err(Error::BodyTooLarge(body_len));
    }
    let len = (body_len as u32).to_be_bytes();
    out[start + 1..start + 4].copy_from_slice(&len[1..]);
    out.extend_from_slice(&((TAG_HEADER_LEN + body_len) as u32).to_be_bytes());
    Ok(out.len() - start)
}

fn composition_time_to_bytes(ms: i32) -> Result<[u8; 3], Error> {
    if !(MIN_COMPOSITION_TIME_MS..=MAX_COMPOSITION_TIME_MS).contains(&ms) {
        return Err(Error::CompositionTimeOutOfRange(ms));
    }
    let v = (ms as u32) & 0xFF_FFFF;
    Ok([(v >> 16) as u8, (v >> 8) as u8, v as u8])
}

fn composition_time_from_bytes(bytes: [u8; 3]) -> i32 {
    let v = (u32::from(bytes[0]) << 16) | (u32::from(bytes[1]) << 8) | u32::from(bytes[2]);
    if v & 0x80_0000 != 0 {
        (v | 0xFF00_0000) as i32
    } else {
        v as i32
    }
}

/// Turns the 32-bit FLV/RTMP timestamp, which wraps after 49.7 days, into a monotonic
/// 64-bit millisecond count. Small backward steps (reordering, clock jitter) stay in the
/// same epoch; a jump backwards by more than half the range is a wrap.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct TimestampUnwrapper {
    last: Option<u32>,
    wraps: u64,
}

impl TimestampUnwrapper {
    /// A fresh unwrapper; the first timestamp defines epoch 0.
    pub fn new() -> Self {
        Self::default()
    }

    /// Maps the next timestamp.
    pub fn unwrap(&mut self, timestamp_ms: u32) -> u64 {
        if let Some(last) = self.last
            && timestamp_ms < last
            && last - timestamp_ms > u32::MAX / 2
        {
            self.wraps += 1;
        }
        self.last = Some(timestamp_ms);
        (self.wraps << 32) | u64::from(timestamp_ms)
    }
}

/// Which streams a muxed file carries.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MuxConfig {
    pub video: bool,
    pub audio: bool,
}

/// Stream description written into the `onMetaData` script tag.
#[derive(Debug, Clone, PartialEq)]
pub struct Metadata {
    pub width: u32,
    pub height: u32,
    pub framerate: f64,
    pub video_kbps: f64,
    pub audio_kbps: f64,
    pub audio_sample_rate: u32,
    pub stereo: bool,
    pub encoder: String,
}

impl Metadata {
    /// Builds the `onMetaData` value for the streams in `config`.
    pub fn to_amf0(&self, config: MuxConfig) -> Value {
        let mut props: Vec<(String, Value)> = Vec::with_capacity(13);
        let mut push = |name: &str, value: Value| props.push((name.to_owned(), value));
        push("duration", Value::Number(0.0));
        if config.video {
            push("width", Value::Number(f64::from(self.width)));
            push("height", Value::Number(f64::from(self.height)));
            push("videodatarate", Value::Number(self.video_kbps));
            push("framerate", Value::Number(self.framerate));
            push("videocodecid", Value::Number(f64::from(CODEC_ID_AVC)));
        }
        if config.audio {
            push("audiodatarate", Value::Number(self.audio_kbps));
            push(
                "audiosamplerate",
                Value::Number(f64::from(self.audio_sample_rate)),
            );
            push("audiosamplesize", Value::Number(16.0));
            push("stereo", Value::Boolean(self.stereo));
            push("audiocodecid", Value::Number(f64::from(SOUND_FORMAT_AAC)));
        }
        push("encoder", Value::String(self.encoder.clone()));
        Value::EcmaArray(props)
    }
}

/// Appends a standalone `onMetaData` script tag (timestamp 0) describing `metadata` for the
/// streams in `config`. The server uses it for recordings and RTMP sessions, where the
/// browser sends media tags only; [`Muxer::write_metadata`] produces the same bytes.
pub fn write_metadata_tag(
    metadata: &Metadata,
    config: MuxConfig,
    out: &mut Vec<u8>,
) -> Result<usize, Error> {
    write_tag_with(TagKind::Script, 0, out, |out| {
        write_metadata_body(metadata, config, out)
    })
}

fn write_metadata_body(
    metadata: &Metadata,
    config: MuxConfig,
    out: &mut Vec<u8>,
) -> Result<(), Error> {
    amf0::write_string("onMetaData", out)?;
    amf0::write_value(&metadata.to_amf0(config), out)?;
    Ok(())
}

/// FLV writer for one H.264 + AAC stream. Every method appends to the caller's buffer and
/// returns the number of bytes appended; on error nothing is appended.
#[derive(Debug)]
pub struct Muxer {
    config: MuxConfig,
    tags_written: u64,
    bytes_written: u64,
}

impl Muxer {
    /// Creates a muxer for the given streams.
    pub fn new(config: MuxConfig) -> Self {
        Muxer {
            config,
            tags_written: 0,
            bytes_written: 0,
        }
    }

    /// The streams this muxer was created for.
    pub fn config(&self) -> MuxConfig {
        self.config
    }

    /// Tags written so far (the file header is not a tag).
    pub fn tags_written(&self) -> u64 {
        self.tags_written
    }

    /// Bytes written so far, header included.
    pub fn bytes_written(&self) -> u64 {
        self.bytes_written
    }

    /// Appends the file header and `PreviousTagSize0`.
    pub fn write_header(&mut self, out: &mut Vec<u8>) -> usize {
        let n = Header {
            has_audio: self.config.audio,
            has_video: self.config.video,
        }
        .write(out);
        self.bytes_written += n as u64;
        n
    }

    /// Appends the `onMetaData` script tag (timestamp 0).
    pub fn write_metadata(
        &mut self,
        metadata: &Metadata,
        out: &mut Vec<u8>,
    ) -> Result<usize, Error> {
        let config = self.config;
        self.tag(TagKind::Script, 0, out, |out| {
            write_metadata_body(metadata, config, out)
        })
    }

    /// Appends the video sequence header carrying the AVCDecoderConfigurationRecord
    /// (what WebCodecs delivers as `decoderConfig.description`).
    pub fn write_video_sequence_header(
        &mut self,
        timestamp_ms: u32,
        avc_decoder_configuration_record: &[u8],
        out: &mut Vec<u8>,
    ) -> Result<usize, Error> {
        self.video_tag(
            timestamp_ms,
            FrameType::Key,
            AvcPacketType::SequenceHeader,
            0,
            avc_decoder_configuration_record,
            out,
        )
    }

    /// Appends one encoded H.264 access unit (length-prefixed NAL units, AVCC format).
    pub fn write_video(
        &mut self,
        timestamp_ms: u32,
        keyframe: bool,
        composition_time_ms: i32,
        nalus: &[u8],
        out: &mut Vec<u8>,
    ) -> Result<usize, Error> {
        let frame_type = if keyframe {
            FrameType::Key
        } else {
            FrameType::Inter
        };
        self.video_tag(
            timestamp_ms,
            frame_type,
            AvcPacketType::Nalu,
            composition_time_ms,
            nalus,
            out,
        )
    }

    /// Appends the AVC end-of-sequence tag.
    pub fn write_video_end_of_sequence(
        &mut self,
        timestamp_ms: u32,
        out: &mut Vec<u8>,
    ) -> Result<usize, Error> {
        self.video_tag(
            timestamp_ms,
            FrameType::Key,
            AvcPacketType::EndOfSequence,
            0,
            &[],
            out,
        )
    }

    /// Appends the audio sequence header carrying the AudioSpecificConfig (what WebCodecs
    /// delivers as `decoderConfig.description` for `mp4a.40.2`).
    pub fn write_audio_sequence_header(
        &mut self,
        timestamp_ms: u32,
        audio_specific_config: &[u8],
        out: &mut Vec<u8>,
    ) -> Result<usize, Error> {
        self.audio_tag(
            timestamp_ms,
            AacPacketType::SequenceHeader,
            audio_specific_config,
            out,
        )
    }

    /// Appends one raw AAC frame (no ADTS header).
    pub fn write_audio(
        &mut self,
        timestamp_ms: u32,
        aac_frame: &[u8],
        out: &mut Vec<u8>,
    ) -> Result<usize, Error> {
        self.audio_tag(timestamp_ms, AacPacketType::Raw, aac_frame, out)
    }

    fn video_tag(
        &mut self,
        timestamp_ms: u32,
        frame_type: FrameType,
        packet_type: AvcPacketType,
        composition_time_ms: i32,
        data: &[u8],
        out: &mut Vec<u8>,
    ) -> Result<usize, Error> {
        let cts = composition_time_to_bytes(composition_time_ms)?;
        self.tag(TagKind::Video, timestamp_ms, out, |out| {
            out.push((frame_type.as_u8() << 4) | CODEC_ID_AVC);
            out.push(packet_type.as_u8());
            out.extend_from_slice(&cts);
            out.extend_from_slice(data);
            Ok(())
        })
    }

    fn audio_tag(
        &mut self,
        timestamp_ms: u32,
        packet_type: AacPacketType,
        data: &[u8],
        out: &mut Vec<u8>,
    ) -> Result<usize, Error> {
        let packet_type = match packet_type {
            AacPacketType::SequenceHeader => 0,
            AacPacketType::Raw => 1,
        };
        self.tag(TagKind::Audio, timestamp_ms, out, |out| {
            out.push(AAC_SOUND_HEADER);
            out.push(packet_type);
            out.extend_from_slice(data);
            Ok(())
        })
    }

    fn tag(
        &mut self,
        kind: TagKind,
        timestamp_ms: u32,
        out: &mut Vec<u8>,
        body: impl FnOnce(&mut Vec<u8>) -> Result<(), Error>,
    ) -> Result<usize, Error> {
        let n = write_tag_with(kind, timestamp_ms, out, body)?;
        self.tags_written += 1;
        self.bytes_written += n as u64;
        Ok(n)
    }
}

/// Limits of the incremental reader.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DemuxLimits {
    /// Largest tag body accepted; larger tags fail on their header.
    pub max_body: usize,
    /// Most bytes buffered without a complete tag before the reader fails.
    pub max_pending: usize,
}

impl Default for DemuxLimits {
    fn default() -> Self {
        DemuxLimits {
            max_body: DEFAULT_MAX_BODY_LEN,
            max_pending: DEFAULT_MAX_PENDING,
        }
    }
}

/// Incremental reader: feed bytes in any chunking, pull whole tags out. Used for recorded
/// files and for the server's ingest stream. Input is untrusted: bodies larger than the
/// limit are rejected on their header, the buffer is bounded, and the first error is
/// sticky (every later call returns it again and the buffer is dropped).
#[derive(Debug)]
pub struct Demuxer {
    buf: BytesMut,
    header: Option<Header>,
    expect_header: bool,
    limits: DemuxLimits,
    error: Option<Error>,
}

impl Default for Demuxer {
    fn default() -> Self {
        Demuxer::without_header()
    }
}

impl Demuxer {
    /// A reader for a stream that starts with the file header.
    pub fn new() -> Self {
        Demuxer {
            expect_header: true,
            ..Demuxer::without_header()
        }
    }

    /// A reader for a bare tag stream (no file header).
    pub fn without_header() -> Self {
        Demuxer {
            buf: BytesMut::new(),
            header: None,
            expect_header: false,
            limits: DemuxLimits::default(),
            error: None,
        }
    }

    /// Replaces the default limits.
    pub fn with_limits(mut self, limits: DemuxLimits) -> Self {
        self.limits = limits;
        self
    }

    /// The limits in force.
    pub fn limits(&self) -> DemuxLimits {
        self.limits
    }

    /// The file header, once it has been parsed.
    pub fn header(&self) -> Option<Header> {
        self.header
    }

    /// Bytes buffered but not yet consumed.
    pub fn pending(&self) -> usize {
        self.buf.len()
    }

    /// The sticky error, if the reader has failed.
    pub fn error(&self) -> Option<&Error> {
        self.error.as_ref()
    }

    /// Appends input. Fails (stickily) when the pending bytes would exceed the limit.
    pub fn feed(&mut self, bytes: &[u8]) -> Result<(), Error> {
        if let Some(e) = &self.error {
            return Err(e.clone());
        }
        let pending = self.buf.len().saturating_add(bytes.len());
        if pending > self.limits.max_pending {
            return Err(self.fail(Error::PendingTooLarge {
                pending,
                limit: self.limits.max_pending,
            }));
        }
        self.buf.extend_from_slice(bytes);
        Ok(())
    }

    /// Returns the next complete tag, or `Ok(None)` when more input is needed.
    pub fn next_tag(&mut self) -> Result<Option<TagBuf>, Error> {
        if let Some(e) = &self.error {
            return Err(e.clone());
        }
        if self.expect_header && self.header.is_none() {
            match Header::parse(&self.buf) {
                Err(e) => return Err(self.fail(e)),
                Ok(None) => return Ok(None),
                Ok(Some((header, n))) => {
                    self.header = Some(header);
                    let _ = self.buf.split_to(n);
                }
            }
        }
        let (kind, timestamp_ms, body_len, total) =
            match parse_tag_with_limit(&self.buf, self.limits.max_body) {
                Err(e) => return Err(self.fail(e)),
                Ok(None) => return Ok(None),
                Ok(Some((tag, n))) => (tag.kind, tag.timestamp_ms, tag.body.len(), n),
            };
        let raw = self.buf.split_to(total).freeze();
        Ok(Some(TagBuf {
            kind,
            timestamp_ms,
            body: raw.slice(TAG_HEADER_LEN..TAG_HEADER_LEN + body_len),
        }))
    }

    /// Pulls every complete tag out of the buffer.
    pub fn drain_tags(&mut self) -> Result<Vec<TagBuf>, Error> {
        let mut tags = Vec::new();
        while let Some(tag) = self.next_tag()? {
            tags.push(tag);
        }
        Ok(tags)
    }

    fn fail(&mut self, error: Error) -> Error {
        self.buf = BytesMut::new();
        self.error = Some(error.clone());
        error
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn metadata() -> Metadata {
        Metadata {
            width: 1920,
            height: 1080,
            framerate: 30.0,
            video_kbps: 6000.0,
            audio_kbps: 128.0,
            audio_sample_rate: 48_000,
            stereo: true,
            encoder: "prostor".into(),
        }
    }

    fn muxer(video: bool, audio: bool) -> Muxer {
        Muxer::new(MuxConfig { video, audio })
    }

    #[test]
    fn header_round_trips() {
        let mut out = Vec::new();
        let header = Header {
            has_audio: true,
            has_video: true,
        };
        assert_eq!(header.write(&mut out), 13);
        assert_eq!(out, [b'F', b'L', b'V', 1, 5, 0, 0, 0, 9, 0, 0, 0, 0]);
        assert_eq!(Header::parse(&out).unwrap(), Some((header, 13)));
        assert_eq!(Header::parse(&out[..12]).unwrap(), None);
        assert_eq!(
            Header::parse(b"FLX\x01\x05\0\0\0\x09"),
            Err(Error::BadSignature)
        );
    }

    #[test]
    fn header_rejects_bad_versions_and_offsets_on_nine_bytes() {
        assert_eq!(
            Header::parse(b"FLV\x02\x05\0\0\0\x09"),
            Err(Error::UnsupportedVersion(2))
        );
        assert_eq!(
            Header::parse(b"FLV\x01\x05\0\0\0\x08"),
            Err(Error::BadDataOffset(8))
        );
        assert_eq!(
            Header::parse(b"FLV\x01\x05\xff\xff\xff\xff"),
            Err(Error::BadDataOffset(0xFFFF_FFFF)),
            "rejected immediately, not after 4 GiB"
        );
        assert_eq!(
            Header::parse(b"FLV\x01\x05\0\0\0\x41"),
            Err(Error::BadDataOffset(65))
        );
        // A larger but sane offset skips the reserved bytes.
        let mut input = b"FLV\x01\x05\0\0\0\x14".to_vec();
        input.extend_from_slice(&[0xAA; 20 - 9]);
        input.extend_from_slice(&[0, 0, 0, 0]);
        assert_eq!(
            Header::parse(&input).unwrap().map(|(_, n)| n),
            Some(24),
            "data offset 20 consumes 24 bytes"
        );
        assert_eq!(Header::parse(&input[..23]).unwrap(), None);
    }

    #[test]
    fn video_tag_layout_matches_the_spec() {
        let mut muxer = muxer(true, false);
        let mut out = Vec::new();
        let n = muxer
            .write_video(0x0102_0304, true, 67, &[0xAA, 0xBB], &mut out)
            .unwrap();
        assert_eq!(n, out.len());
        assert_eq!(
            out,
            [
                9, 0, 0, 7, // video, body size 7
                0x02, 0x03, 0x04, 0x01, // timestamp low 24 bits, then extended byte
                0, 0, 0, // stream id
                0x17, 1, 0, 0, 67, 0xAA, 0xBB, // key frame, AVC, NALU, cts 67, data
                0, 0, 0, 18 // previous tag size = 11 + 7
            ]
        );
        let (tag, consumed) = parse_tag(&out).unwrap().unwrap();
        assert_eq!(consumed, out.len());
        assert_eq!(tag.timestamp_ms, 0x0102_0304);
        let video = tag.video().unwrap();
        assert_eq!(video.frame_type, FrameType::Key);
        assert_eq!(video.packet_type, AvcPacketType::Nalu);
        assert_eq!(video.composition_time_ms, 67);
        assert_eq!(video.data, &[0xAA, 0xBB]);
        assert_eq!(tag.class(), Ok(TagClass::Key));
        assert_eq!(muxer.tags_written(), 1);
        assert_eq!(muxer.bytes_written(), out.len() as u64);
    }

    #[test]
    fn timestamps_cover_the_whole_32_bit_range() {
        for ts in [0, 0x00FF_FFFF, 0x0100_0000, 0x7FFF_FFFF, u32::MAX] {
            let mut out = Vec::new();
            write_tag(TagKind::Audio, ts, &[0xAF, 1, 9], &mut out).unwrap();
            let (tag, n) = parse_tag(&out).unwrap().unwrap();
            assert_eq!(n, out.len());
            assert_eq!(tag.timestamp_ms, ts, "timestamp {ts:#x}");
        }
    }

    #[test]
    fn audio_tag_layout_matches_the_spec() {
        let mut muxer = muxer(false, true);
        let mut out = Vec::new();
        muxer
            .write_audio_sequence_header(0, &[0x11, 0x90], &mut out)
            .unwrap();
        assert_eq!(&out[11..15], &[0xAF, 0, 0x11, 0x90]);
        assert_eq!(
            parse_tag(&out).unwrap().unwrap().0.class(),
            Ok(TagClass::SequenceHeader)
        );
        out.clear();
        muxer.write_audio(21, &[1, 2, 3], &mut out).unwrap();
        assert_eq!(&out[11..16], &[0xAF, 1, 1, 2, 3]);
        let (tag, _) = parse_tag(&out).unwrap().unwrap();
        let audio = tag.audio().unwrap();
        assert_eq!(audio.packet_type, AacPacketType::Raw);
        assert_eq!(audio.data, &[1, 2, 3]);
        assert_eq!(tag.class(), Ok(TagClass::Audio));
        assert_eq!(tag.video(), Err(Error::WrongKind(TagKind::Audio)));
    }

    #[test]
    fn classifies_every_tag_kind() {
        let mut muxer = muxer(true, true);
        let mut out = Vec::new();
        muxer.write_metadata(&metadata(), &mut out).unwrap();
        muxer
            .write_video_sequence_header(0, &[1, 0x4d, 0, 0x28], &mut out)
            .unwrap();
        muxer.write_video(0, true, 0, &[0], &mut out).unwrap();
        muxer.write_video(33, false, 0, &[0], &mut out).unwrap();
        muxer.write_video_end_of_sequence(66, &mut out).unwrap();
        muxer
            .write_audio_sequence_header(0, &[0x11, 0x90], &mut out)
            .unwrap();
        muxer.write_audio(0, &[0], &mut out).unwrap();
        let mut demuxer = Demuxer::without_header();
        demuxer.feed(&out).unwrap();
        let classes: Vec<_> = demuxer
            .drain_tags()
            .unwrap()
            .iter()
            .map(|t| t.as_tag().class().unwrap())
            .collect();
        assert_eq!(
            classes,
            [
                TagClass::Script,
                TagClass::SequenceHeader,
                TagClass::Key,
                TagClass::Delta,
                TagClass::EndOfSequence,
                TagClass::SequenceHeader,
                TagClass::Audio,
            ]
        );
        // Disposable inter and generated key frames map like their plain counterparts.
        assert_eq!(
            VideoTag::parse(&[0x37, 1, 0, 0, 0]).unwrap().frame_type,
            FrameType::DisposableInter
        );
        assert_eq!(
            Tag {
                kind: TagKind::Video,
                timestamp_ms: 0,
                body: &[0x37, 1, 0, 0, 0]
            }
            .class(),
            Ok(TagClass::Delta)
        );
        assert_eq!(
            Tag {
                kind: TagKind::Video,
                timestamp_ms: 0,
                body: &[0x47, 1, 0, 0, 0]
            }
            .class(),
            Ok(TagClass::Key)
        );
        assert_eq!(
            Tag {
                kind: TagKind::Video,
                timestamp_ms: 0,
                body: &[0x57, 0]
            }
            .class(),
            Err(Error::BadFrameType(5))
        );
    }

    #[test]
    fn video_and_audio_bodies_are_validated() {
        assert_eq!(VideoTag::parse(&[]), Err(Error::TruncatedBody));
        assert_eq!(VideoTag::parse(&[0x17, 1, 0, 0]), Err(Error::TruncatedBody));
        assert_eq!(
            VideoTag::parse(&[0x12, 1, 0, 0, 0]),
            Err(Error::UnsupportedCodec(2))
        );
        assert_eq!(
            VideoTag::parse(&[0x17, 3, 0, 0, 0]),
            Err(Error::BadPacketType(3))
        );
        assert_eq!(
            VideoTag::parse(&[0x07, 1, 0, 0, 0]),
            Err(Error::BadFrameType(0))
        );
        assert_eq!(
            VideoTag::parse(&[0x57, 1]),
            Err(Error::BadFrameType(5)),
            "command frame"
        );
        assert_eq!(
            VideoTag::parse(&[0x67, 1, 0, 0, 0]),
            Err(Error::BadFrameType(6))
        );
        assert_eq!(
            VideoTag::parse(&[0x97, b'a', b'v', b'c', b'1']),
            Err(Error::EnhancedRtmpVideo),
            "ex-header bit set"
        );
        assert_eq!(AudioTag::parse(&[]), Err(Error::TruncatedBody));
        assert_eq!(AudioTag::parse(&[0xAF]), Err(Error::TruncatedBody));
        assert_eq!(
            AudioTag::parse(&[0x2F, 1]),
            Err(Error::UnsupportedCodec(2)),
            "mp3"
        );
        assert_eq!(AudioTag::parse(&[0xAF, 2]), Err(Error::BadPacketType(2)));
        assert_eq!(
            AudioTag::parse(&[0xAF, 0]).unwrap().packet_type,
            AacPacketType::SequenceHeader
        );
        assert_eq!(
            Tag {
                kind: TagKind::Audio,
                timestamp_ms: 0,
                body: &[0x2F, 1]
            }
            .class(),
            Err(Error::UnsupportedCodec(2))
        );
    }

    #[test]
    fn composition_time_is_signed_24_bit_and_range_checked() {
        for ms in [
            0,
            1,
            67,
            -1,
            -33,
            MAX_COMPOSITION_TIME_MS,
            MIN_COMPOSITION_TIME_MS,
        ] {
            assert_eq!(
                composition_time_from_bytes(composition_time_to_bytes(ms).unwrap()),
                ms,
                "cts {ms}"
            );
        }
        let mut muxer = muxer(true, false);
        let mut out = Vec::new();
        muxer.write_video(10, false, -33, &[0], &mut out).unwrap();
        assert_eq!(&out[13..16], &[0xFF, 0xFF, 0xDF]);
        assert_eq!(
            parse_tag(&out)
                .unwrap()
                .unwrap()
                .0
                .video()
                .unwrap()
                .composition_time_ms,
            -33
        );
        for ms in [
            MAX_COMPOSITION_TIME_MS + 1,
            MIN_COMPOSITION_TIME_MS - 1,
            i32::MAX,
            i32::MIN,
        ] {
            let mut out = vec![0xEE];
            assert_eq!(
                muxer.write_video(10, false, ms, &[0], &mut out),
                Err(Error::CompositionTimeOutOfRange(ms))
            );
            assert_eq!(out, [0xEE], "nothing appended for cts {ms}");
        }
        assert_eq!(muxer.tags_written(), 1);
    }

    #[test]
    fn metadata_tag_decodes_back() {
        let config = MuxConfig {
            video: true,
            audio: true,
        };
        let mut muxer = Muxer::new(config);
        let mut out = Vec::new();
        muxer.write_metadata(&metadata(), &mut out).unwrap();
        let (tag, _) = parse_tag(&out).unwrap().unwrap();
        assert_eq!(tag.kind, TagKind::Script);
        assert_eq!(tag.timestamp_ms, 0);
        assert_eq!(tag.class(), Ok(TagClass::Script));
        let (name, value) = parse_script_tag(tag.body).unwrap();
        assert_eq!(name, "onMetaData");
        assert_eq!(value, metadata().to_amf0(config));
        assert_eq!(value.get("width").and_then(Value::as_f64), Some(1920.0));
        assert_eq!(
            value.get("audiocodecid").and_then(Value::as_f64),
            Some(10.0)
        );
        assert_eq!(value.get("stereo").and_then(Value::as_bool), Some(true));

        let mut standalone = Vec::new();
        let n = write_metadata_tag(&metadata(), config, &mut standalone).unwrap();
        assert_eq!(n, standalone.len());
        assert_eq!(
            standalone, out,
            "the standalone writer produces the muxer's bytes"
        );
        assert_eq!(
            muxer.tags_written(),
            1,
            "the standalone writer counts nothing"
        );

        let video_only = MuxConfig {
            video: true,
            audio: false,
        };
        let mut without_audio = Vec::new();
        write_metadata_tag(&metadata(), video_only, &mut without_audio).unwrap();
        let (_, value) = parse_script_tag(parse_exact(&without_audio).unwrap().body).unwrap();
        assert!(value.get("audiocodecid").is_none());
        assert_eq!(value.get("videocodecid").and_then(Value::as_f64), Some(7.0));
    }

    #[test]
    fn script_tags_are_size_and_shape_checked() {
        let big = vec![0u8; MAX_SCRIPT_BODY_LEN + 1];
        assert_eq!(
            parse_script_tag(&big),
            Err(Error::ScriptTooLarge(MAX_SCRIPT_BODY_LEN + 1))
        );
        assert_eq!(
            parse_script_tag(&[0x05, 0x05]),
            Err(Error::Amf0(amf0::Error::UnknownMarker(5))),
            "name must be a string"
        );
        let mut body = Vec::new();
        amf0::write_string("onMetaData", &mut body).unwrap();
        amf0::write_value(&Value::Null, &mut body).unwrap();
        body.push(0x05);
        assert_eq!(parse_script_tag(&body), Err(Error::TrailingBytes(1)));
        assert_eq!(
            parse_script_tag(&body[..body.len() - 2]),
            Err(Error::Amf0(amf0::Error::UnexpectedEof))
        );
    }

    #[test]
    fn rejects_oversized_bodies_without_leaving_partial_output() {
        let mut muxer = muxer(true, false);
        let mut out = vec![0xEE];
        let big = vec![0u8; MAX_BODY_LEN - 4];
        assert_eq!(
            muxer.write_video(0, true, 0, &big, &mut out),
            Err(Error::BodyTooLarge(MAX_BODY_LEN + 1))
        );
        assert_eq!(out, [0xEE]);
        assert_eq!(muxer.tags_written(), 0);
    }

    #[test]
    fn bodies_of_exactly_the_maximum_and_of_zero_length_round_trip() {
        let mut out = Vec::new();
        let max = vec![0x5A; MAX_BODY_LEN];
        let n = write_tag(TagKind::Script, 1, &max, &mut out).unwrap();
        assert_eq!(n, TAG_HEADER_LEN + MAX_BODY_LEN + PREVIOUS_TAG_SIZE_LEN);
        let (tag, consumed) = parse_tag(&out).unwrap().unwrap();
        assert_eq!(consumed, n);
        assert_eq!(tag.body.len(), MAX_BODY_LEN);

        out.clear();
        write_tag(TagKind::Script, 0, &[], &mut out).unwrap();
        assert_eq!(out, [18, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 11]);
        let (tag, consumed) = parse_tag(&out).unwrap().unwrap();
        assert_eq!(consumed, 15);
        assert!(tag.body.is_empty());
        assert_eq!(parse_exact(&out).unwrap().kind, TagKind::Script);
    }

    #[test]
    fn parser_waits_for_whole_tags_and_validates_trailers() {
        let mut muxer = muxer(true, true);
        let mut out = Vec::new();
        muxer.write_video(1, true, 0, &[7; 40], &mut out).unwrap();
        for cut in 0..out.len() {
            assert_eq!(parse_tag(&out[..cut]).unwrap(), None, "cut at {cut}");
            assert_eq!(
                parse_exact(&out[..cut]),
                Err(Error::Truncated),
                "cut at {cut}"
            );
        }
        let mut broken = out.clone();
        let last = broken.len() - 1;
        broken[last] ^= 1;
        assert!(matches!(
            parse_tag(&broken),
            Err(Error::BadPreviousTagSize { .. })
        ));
        let mut encrypted = out.clone();
        encrypted[0] |= 0b0010_0000;
        assert_eq!(parse_tag(&encrypted), Err(Error::EncryptedTag));
        let mut unknown = out.clone();
        unknown[0] = 3;
        assert_eq!(parse_tag(&unknown), Err(Error::UnknownTagType(3)));

        let mut trailing = out.clone();
        trailing.extend_from_slice(&[1, 2, 3]);
        assert_eq!(parse_exact(&trailing), Err(Error::TrailingBytes(3)));
        assert_eq!(parse_exact(&out).unwrap().timestamp_ms, 1);
    }

    #[test]
    fn parser_limit_is_enforced_on_the_tag_header() {
        let mut header = vec![9, 0xFF, 0xFF, 0xFF, 0, 0, 0, 0, 0, 0, 0];
        assert_eq!(
            parse_tag_with_limit(&header, DEFAULT_MAX_BODY_LEN),
            Err(Error::BodyExceedsLimit {
                len: MAX_BODY_LEN,
                limit: DEFAULT_MAX_BODY_LEN
            }),
            "rejected before any body byte arrives"
        );
        header[1] = 0;
        header[2] = 0;
        header[3] = 10;
        assert_eq!(
            parse_tag_with_limit(&header, 10).unwrap(),
            None,
            "exactly the limit is fine"
        );
        assert_eq!(
            parse_tag_with_limit(&header, 9),
            Err(Error::BodyExceedsLimit { len: 10, limit: 9 })
        );
    }

    fn sample_stream() -> Vec<u8> {
        let mut muxer = muxer(true, true);
        let mut stream = Vec::new();
        muxer.write_header(&mut stream);
        muxer.write_metadata(&metadata(), &mut stream).unwrap();
        for i in 0..20u32 {
            muxer
                .write_video(
                    i * 33,
                    i % 10 == 0,
                    0,
                    &vec![i as u8; 100 + i as usize],
                    &mut stream,
                )
                .unwrap();
            muxer.write_audio(i * 21, &[0xAB; 50], &mut stream).unwrap();
        }
        stream
    }

    #[test]
    fn demuxer_handles_odd_chunk_boundaries() {
        let stream = sample_stream();
        for chunk in [1usize, 7, 64, 1000, usize::MAX] {
            let mut demuxer = Demuxer::new();
            let mut tags = Vec::new();
            for piece in stream.chunks(chunk.min(stream.len())) {
                demuxer.feed(piece).unwrap();
                while let Some(tag) = demuxer.next_tag().unwrap() {
                    tags.push(tag);
                }
            }
            assert_eq!(
                demuxer.header(),
                Some(Header {
                    has_audio: true,
                    has_video: true
                })
            );
            assert_eq!(tags.len(), 41, "chunk size {chunk}");
            assert_eq!(tags[0].kind, TagKind::Script);
            assert_eq!(tags[1].as_tag().video().unwrap().data, &[0u8; 100]);
            assert_eq!(tags[40].timestamp_ms, 19 * 21);
            assert_eq!(demuxer.pending(), 0);
            assert_eq!(demuxer.error(), None);
        }
    }

    #[test]
    fn demuxer_drain_matches_next_tag() {
        let stream = sample_stream();
        let mut one_by_one = Demuxer::new();
        one_by_one.feed(&stream).unwrap();
        let mut expected = Vec::new();
        while let Some(tag) = one_by_one.next_tag().unwrap() {
            expected.push(tag);
        }
        let mut drained = Demuxer::new();
        drained.feed(&stream).unwrap();
        assert_eq!(drained.drain_tags().unwrap(), expected);
        assert_eq!(drained.drain_tags().unwrap(), Vec::new());
    }

    #[test]
    fn demuxer_without_header_reads_bare_tags_and_rejects_a_header() {
        let mut out = Vec::new();
        write_tag(TagKind::Script, 5, &[1, 2, 3], &mut out).unwrap();
        let mut demuxer = Demuxer::without_header();
        demuxer.feed(&out).unwrap();
        let tag = demuxer.next_tag().unwrap().unwrap();
        assert_eq!(
            (tag.kind, tag.timestamp_ms, &tag.body[..]),
            (TagKind::Script, 5, &[1, 2, 3][..])
        );
        assert_eq!(demuxer.next_tag().unwrap(), None);

        let mut with_header = Demuxer::without_header();
        with_header.feed(&sample_stream()).unwrap();
        assert_eq!(
            with_header.next_tag(),
            Err(Error::UnknownTagType(b'F')),
            "a file header is not a tag"
        );
    }

    #[test]
    fn demuxer_rejects_hostile_headers_and_bodies_immediately() {
        let mut demuxer = Demuxer::new();
        demuxer.feed(b"FLV\x01\x05\xff\xff\xff\xff").unwrap();
        assert_eq!(demuxer.next_tag(), Err(Error::BadDataOffset(0xFFFF_FFFF)));
        assert_eq!(demuxer.pending(), 0, "buffer dropped");

        let mut demuxer = Demuxer::without_header();
        demuxer
            .feed(&[9, 0xFF, 0xFF, 0xFF, 0, 0, 0, 0, 0, 0, 0])
            .unwrap();
        assert_eq!(
            demuxer.next_tag(),
            Err(Error::BodyExceedsLimit {
                len: MAX_BODY_LEN,
                limit: DEFAULT_MAX_BODY_LEN
            })
        );

        let mut tight = Demuxer::without_header().with_limits(DemuxLimits {
            max_body: 100,
            max_pending: 150,
        });
        let mut tag = Vec::new();
        write_tag(TagKind::Script, 0, &[0; 100], &mut tag).unwrap();
        tight.feed(&tag).unwrap();
        assert!(
            tight.next_tag().unwrap().is_some(),
            "exactly the limit passes"
        );
        assert_eq!(
            tight.feed(&[0; 151]),
            Err(Error::PendingTooLarge {
                pending: 151,
                limit: 150
            })
        );
    }

    #[test]
    fn demuxer_errors_are_sticky() {
        let mut demuxer = Demuxer::without_header();
        let mut good = Vec::new();
        write_tag(TagKind::Script, 0, &[1], &mut good).unwrap();
        let mut bad = good.clone();
        bad[0] |= 0b0010_0000;
        demuxer.feed(&bad).unwrap();
        assert_eq!(demuxer.next_tag(), Err(Error::EncryptedTag));
        assert_eq!(demuxer.error(), Some(&Error::EncryptedTag));
        assert_eq!(demuxer.feed(&good), Err(Error::EncryptedTag));
        assert_eq!(demuxer.next_tag(), Err(Error::EncryptedTag));
        assert_eq!(demuxer.drain_tags(), Err(Error::EncryptedTag));
        assert_eq!(demuxer.pending(), 0);
    }

    #[test]
    fn timestamp_unwrapper_handles_wraps_and_jitter() {
        let mut unwrapper = TimestampUnwrapper::new();
        assert_eq!(unwrapper.unwrap(0), 0);
        assert_eq!(unwrapper.unwrap(100), 100);
        assert_eq!(
            unwrapper.unwrap(90),
            90,
            "small backward step stays in the epoch"
        );
        assert_eq!(unwrapper.unwrap(u32::MAX - 10), u64::from(u32::MAX - 10));
        assert_eq!(unwrapper.unwrap(5), (1 << 32) + 5, "wrapped");
        assert_eq!(
            unwrapper.unwrap(u32::MAX),
            (1 << 32) + u64::from(u32::MAX),
            "still epoch 1 (jitter)"
        );
        assert_eq!(unwrapper.unwrap(3), (2 << 32) + 3);
    }
}
