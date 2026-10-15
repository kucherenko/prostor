//! An FLV recording on the server's disk.
//!
//! The file is `<room>-<UTC yyyymmdd-hhmmss>.flv` in the recordings directory, created
//! when the session opens (so a failure surfaces before `started` is sent) and written
//! through a `BufWriter`; `stop` flushes and syncs it. Only the file name is ever
//! reported to the browser.

use std::{
    io,
    path::{Path, PathBuf},
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use async_trait::async_trait;
use prostor_model::EgressKind;
use prostor_protocol::flv::{Header, TagBuf};
use tokio::{
    fs::{File, OpenOptions},
    io::{AsyncWriteExt, BufWriter},
};

use super::{Delivery, Egress, EgressError, StreamHeaders};
use crate::room::RoomId;

/// Id of the recording egress in status reports.
pub const FILE_EGRESS_ID: &str = "file";
/// Write buffer: a few video frames.
const BUFFER_LEN: usize = 512 * 1024;
/// Most files tried for one `<room>-<stamp>` before giving up.
const MAX_NAME_ATTEMPTS: u32 = 100;

/// Writes the session to one FLV file.
pub struct FileEgress {
    name: String,
    path: PathBuf,
    file: Option<BufWriter<File>>,
    buf: Vec<u8>,
}

impl FileEgress {
    /// Creates the recording file for `room` in `dir`, named after `now` (UTC). A name
    /// that exists already gets a `-2`, `-3`, ... suffix.
    pub async fn create(dir: &Path, room: &RoomId, now: SystemTime) -> io::Result<Self> {
        let stem = format!("{room}-{}", utc_stamp(now));
        for attempt in 1..=MAX_NAME_ATTEMPTS {
            let name = if attempt == 1 {
                format!("{stem}.flv")
            } else {
                format!("{stem}-{attempt}.flv")
            };
            let path = dir.join(&name);
            match OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&path)
                .await
            {
                Ok(file) => {
                    return Ok(FileEgress {
                        name,
                        path,
                        file: Some(BufWriter::with_capacity(BUFFER_LEN, file)),
                        buf: Vec::with_capacity(BUFFER_LEN),
                    });
                }
                Err(e) if e.kind() == io::ErrorKind::AlreadyExists => continue,
                Err(e) => return Err(e),
            }
        }
        Err(io::Error::new(
            io::ErrorKind::AlreadyExists,
            format!("{MAX_NAME_ATTEMPTS} recordings named {stem}*.flv exist already"),
        ))
    }

    /// The file name (no directory), as reported to the browser.
    pub fn file_name(&self) -> &str {
        &self.name
    }

    /// The full path of the recording.
    pub fn path(&self) -> &Path {
        &self.path
    }

    async fn write_buf(&mut self) -> Result<(), EgressError> {
        let file = self.file.as_mut().ok_or(EgressError::Closed)?;
        file.write_all(&self.buf).await?;
        self.buf.clear();
        Ok(())
    }
}

#[async_trait]
impl Egress for FileEgress {
    fn id(&self) -> &str {
        FILE_EGRESS_ID
    }

    fn kind(&self) -> EgressKind {
        EgressKind::File
    }

    async fn start(&mut self, headers: &StreamHeaders) -> Result<(), EgressError> {
        let config = headers.mux_config();
        self.buf.clear();
        Header {
            has_audio: config.audio,
            has_video: config.video,
        }
        .write(&mut self.buf);
        headers.metadata.as_tag().write(&mut self.buf)?;
        headers
            .video_sequence_header
            .as_tag()
            .write(&mut self.buf)?;
        if let Some(audio) = &headers.audio_sequence_header {
            audio.as_tag().write(&mut self.buf)?;
        }
        self.write_buf().await
    }

    async fn push(&mut self, _seq: u32, tag: &TagBuf) -> Result<Delivery, EgressError> {
        self.buf.clear();
        tag.as_tag().write(&mut self.buf)?;
        self.write_buf().await?;
        Ok(Delivery::Sent)
    }

    async fn stop(&mut self) -> Result<(), EgressError> {
        let Some(mut file) = self.file.take() else {
            return Err(EgressError::Closed);
        };
        file.flush().await?;
        file.get_ref().sync_all().await?;
        Ok(())
    }
}

/// `yyyymmdd-hhmmss` in UTC. Times before the epoch clamp to the epoch.
pub fn utc_stamp(time: SystemTime) -> String {
    let secs = time
        .duration_since(UNIX_EPOCH)
        .unwrap_or(Duration::ZERO)
        .as_secs();
    let (days, rem) = (secs / 86_400, secs % 86_400);
    let (hour, minute, second) = (rem / 3600, (rem % 3600) / 60, rem % 60);
    let (year, month, day) = civil_from_days(days as i64);
    format!("{year:04}{month:02}{day:02}-{hour:02}{minute:02}{second:02}")
}

/// Proleptic Gregorian date from days since 1970-01-01 (Howard Hinnant's algorithm).
fn civil_from_days(days: i64) -> (i64, u32, u32) {
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let month = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    let year = yoe + era * 400 + i64::from(month <= 2);
    (year, month, day)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::egress::metadata_tag;
    use prostor_model::StreamInfo;
    use prostor_protocol::flv::{Demuxer, TagKind, parse_script_tag, write_tag};

    fn at(secs: u64) -> SystemTime {
        UNIX_EPOCH + Duration::from_secs(secs)
    }

    #[test]
    fn stamps_are_utc_and_zero_padded() {
        assert_eq!(utc_stamp(at(0)), "19700101-000000");
        assert_eq!(utc_stamp(at(1_000_000_000)), "20010909-014640");
        assert_eq!(utc_stamp(at(951_782_400)), "20000229-000000", "leap day");
        assert_eq!(utc_stamp(at(1_792_001_700)), "20261014-181500");
        assert_eq!(
            utc_stamp(UNIX_EPOCH - Duration::from_secs(5)),
            "19700101-000000"
        );
    }

    fn info() -> StreamInfo {
        StreamInfo {
            width: 1280,
            height: 720,
            fps: 30.0,
            video_codec: "avc1.4d001f".into(),
            video_bitrate: 2_000_000,
            audio_codec: Some("mp4a.40.2".into()),
            audio_bitrate: Some(128_000),
            sample_rate: Some(48_000),
            channels: Some(2),
        }
    }

    fn tag(kind: TagKind, ts: u32, body: &[u8]) -> TagBuf {
        let mut bytes = Vec::new();
        write_tag(kind, ts, body, &mut bytes).unwrap();
        prostor_protocol::flv::parse_exact(&bytes)
            .unwrap()
            .to_owned()
    }

    fn headers() -> StreamHeaders {
        StreamHeaders {
            info: info(),
            metadata: metadata_tag(&info()).unwrap(),
            video_sequence_header: tag(TagKind::Video, 0, &[0x17, 0, 0, 0, 0, 1, 0x4d, 0x40, 0x1f]),
            audio_sequence_header: Some(tag(TagKind::Audio, 0, &[0xAF, 0, 0x11, 0x90])),
        }
    }

    #[tokio::test]
    async fn writes_a_parseable_file_with_headers_then_tags() {
        let dir = tempfile::tempdir().unwrap();
        let room = RoomId::parse("studio").unwrap();
        let mut egress = FileEgress::create(dir.path(), &room, at(1_000_000_000))
            .await
            .unwrap();
        assert_eq!(egress.file_name(), "studio-20010909-014640.flv");
        assert_eq!(egress.id(), FILE_EGRESS_ID);
        assert_eq!(egress.kind(), EgressKind::File);

        let headers = headers();
        egress.start(&headers).await.unwrap();
        let frame = tag(TagKind::Video, 33, &[0x27, 1, 0, 0, 0, 0xAA, 0xBB]);
        let audio = tag(TagKind::Audio, 46, &[0xAF, 1, 0xDE, 0xAD]);
        egress.push(1, &frame).await.unwrap();
        egress.push(2, &audio).await.unwrap();
        egress.stop().await.unwrap();
        assert!(matches!(
            egress.push(3, &audio).await,
            Err(EgressError::Closed)
        ));

        let bytes = std::fs::read(egress.path()).unwrap();
        let mut demuxer = Demuxer::new();
        demuxer.feed(&bytes).unwrap();
        let tags = demuxer.drain_tags().unwrap();
        assert_eq!(demuxer.pending(), 0);
        assert_eq!(
            demuxer.header(),
            Some(Header {
                has_audio: true,
                has_video: true
            })
        );
        assert_eq!(tags.len(), 5);
        assert_eq!(tags[0].kind, TagKind::Script);
        let (name, meta) = parse_script_tag(&tags[0].body).unwrap();
        assert_eq!(name, "onMetaData");
        assert_eq!(meta.get("height").and_then(|v| v.as_f64()), Some(720.0));
        assert_eq!(tags[1], headers.video_sequence_header);
        assert_eq!(tags[2], headers.audio_sequence_header.unwrap());
        assert_eq!(tags[3], frame);
        assert_eq!(tags[4], audio);
    }

    #[tokio::test]
    async fn name_collisions_get_a_suffix_and_a_missing_dir_is_an_error() {
        let dir = tempfile::tempdir().unwrap();
        let room = RoomId::parse("r").unwrap();
        let first = FileEgress::create(dir.path(), &room, at(0)).await.unwrap();
        let second = FileEgress::create(dir.path(), &room, at(0)).await.unwrap();
        let third = FileEgress::create(dir.path(), &room, at(0)).await.unwrap();
        assert_eq!(first.file_name(), "r-19700101-000000.flv");
        assert_eq!(second.file_name(), "r-19700101-000000-2.flv");
        assert_eq!(third.file_name(), "r-19700101-000000-3.flv");

        let missing = dir.path().join("missing");
        assert!(FileEgress::create(&missing, &room, at(0)).await.is_err());
    }
}
