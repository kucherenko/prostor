//! Where a session's media goes.
//!
//! An [`Egress`] consumes the FLV tags of one ingest session: a recording on disk now
//! ([`FileEgress`]), RTMP destinations in M2. Every egress starts from the same
//! [`StreamHeaders`] (metadata and the codec sequence headers), which the
//! [`Supervisor`] caches from the first tags of the session and resends on every
//! (re)start, so an egress never needs to inspect the stream itself.

use std::{fmt, io};

use async_trait::async_trait;
use prostor_model::{EgressKind, StreamInfo};
use prostor_protocol::flv::{self, Metadata, MuxConfig, TagBuf, parse_exact, write_metadata_tag};

pub mod file;
pub mod supervisor;

pub use file::FileEgress;
pub use supervisor::{Supervisor, SupervisorError};

/// Everything an egress needs before the first media tag.
#[derive(Debug, Clone, PartialEq)]
pub struct StreamHeaders {
    /// What the browser announced in `hello`.
    pub info: StreamInfo,
    /// The `onMetaData` script tag the server built from `info`.
    pub metadata: TagBuf,
    /// The AVC sequence header (first tag of every session).
    pub video_sequence_header: TagBuf,
    /// The AAC sequence header, if it arrived before the first media frame. A later one
    /// is forwarded as an ordinary tag.
    pub audio_sequence_header: Option<TagBuf>,
}

impl StreamHeaders {
    /// Which streams the session carries, for the FLV file header.
    pub fn mux_config(&self) -> MuxConfig {
        mux_config(&self.info)
    }
}

/// Which streams `info` describes.
pub fn mux_config(info: &StreamInfo) -> MuxConfig {
    MuxConfig {
        video: true,
        audio: info.audio_codec.is_some(),
    }
}

/// Builds the `onMetaData` tag for a stream.
pub fn metadata_tag(info: &StreamInfo) -> Result<TagBuf, flv::Error> {
    let metadata = Metadata {
        width: info.width,
        height: info.height,
        framerate: info.fps,
        video_kbps: f64::from(info.video_bitrate) / 1000.0,
        audio_kbps: f64::from(info.audio_bitrate.unwrap_or(0)) / 1000.0,
        audio_sample_rate: info.sample_rate.unwrap_or(0),
        stereo: info.channels.unwrap_or(0) >= 2,
        encoder: format!("prostor/{}", env!("CARGO_PKG_VERSION")),
    };
    let mut bytes = Vec::new();
    write_metadata_tag(&metadata, mux_config(info), &mut bytes)?;
    Ok(parse_exact(&bytes)?.to_owned())
}

/// Why an egress stopped consuming.
#[derive(Debug)]
pub enum EgressError {
    Io(io::Error),
    Flv(flv::Error),
    /// `push` after `stop`, or after an earlier failure.
    Closed,
}

impl fmt::Display for EgressError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            EgressError::Io(e) => write!(f, "i/o error: {e}"),
            EgressError::Flv(e) => write!(f, "flv error: {e}"),
            EgressError::Closed => write!(f, "egress is closed"),
        }
    }
}

impl std::error::Error for EgressError {}

impl From<io::Error> for EgressError {
    fn from(e: io::Error) -> Self {
        EgressError::Io(e)
    }
}

impl From<flv::Error> for EgressError {
    fn from(e: flv::Error) -> Self {
        EgressError::Flv(e)
    }
}

/// One consumer of a session's tags. Methods are called from a dedicated task, in order:
/// `start` once, `push` per tag, `stop` once. A method that fails ends the egress; the
/// supervisor reports it as failed and never calls it again.
#[async_trait]
pub trait Egress: Send {
    /// Stable within the session, e.g. `file` or `rtmp-<destination id>`.
    fn id(&self) -> &str;

    /// What kind of egress this is, for status reports.
    fn kind(&self) -> EgressKind;

    /// Writes the stream preamble (file header, metadata, sequence headers).
    async fn start(&mut self, headers: &StreamHeaders) -> Result<(), EgressError>;

    /// Consumes one tag. `seq` is the ingest sequence number, for status reports.
    async fn push(&mut self, seq: u32, tag: &TagBuf) -> Result<(), EgressError>;

    /// Finalizes the output (flush, sync, close).
    async fn stop(&mut self) -> Result<(), EgressError>;
}

#[cfg(test)]
mod tests {
    use super::*;
    use prostor_protocol::flv::{TagKind, parse_script_tag};

    fn info(audio: bool) -> StreamInfo {
        StreamInfo {
            width: 1920,
            height: 1080,
            fps: 30.0,
            video_codec: "avc1.4d0028".into(),
            video_bitrate: 6_000_000,
            audio_codec: audio.then(|| "mp4a.40.2".into()),
            audio_bitrate: audio.then_some(128_000),
            sample_rate: audio.then_some(48_000),
            channels: audio.then_some(2),
        }
    }

    #[test]
    fn metadata_tag_describes_the_stream() {
        let tag = metadata_tag(&info(true)).unwrap();
        assert_eq!(tag.kind, TagKind::Script);
        assert_eq!(tag.timestamp_ms, 0);
        let (name, value) = parse_script_tag(&tag.body).unwrap();
        assert_eq!(name, "onMetaData");
        assert_eq!(value.get("width").and_then(|v| v.as_f64()), Some(1920.0));
        assert_eq!(
            value.get("videodatarate").and_then(|v| v.as_f64()),
            Some(6000.0)
        );
        assert_eq!(
            value.get("audiosamplerate").and_then(|v| v.as_f64()),
            Some(48_000.0)
        );
        assert_eq!(value.get("stereo").and_then(|v| v.as_bool()), Some(true));
        assert!(
            value
                .get("encoder")
                .and_then(|v| v.as_str())
                .unwrap()
                .starts_with("prostor/")
        );
    }

    #[test]
    fn video_only_metadata_has_no_audio_fields() {
        let tag = metadata_tag(&info(false)).unwrap();
        let (_, value) = parse_script_tag(&tag.body).unwrap();
        assert!(value.get("audiocodecid").is_none());
        assert!(value.get("videocodecid").is_some());
        assert_eq!(
            mux_config(&info(false)),
            MuxConfig {
                video: true,
                audio: false
            }
        );
    }
}
