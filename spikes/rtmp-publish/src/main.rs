//! Spike (b): publish an FLV file to an RTMP server through `prostor_protocol::rtmp`.
//!
//! ```text
//! rtmp-publish --input fixtures/testsrc-720p.flv [--url rtmp://host:port/app/key] [--realtime] [--loop N]
//! ```
//!
//! The URL may also come from `PROSTOR_SPIKE_RTMP_URL`, which is the only sane way to pass a
//! real stream key (it never appears in `ps` output). The key is never printed: every log
//! line goes through the redacting `Display` of `Target`, and server text in events is
//! scrubbed by the session.

use prostor_protocol::flv::{Header, Metadata, Tag, TagKind, parse_script_tag, parse_tag};
use prostor_protocol::rtmp::{Event, Output, Publisher, State, Target};
use std::io::ErrorKind;
use std::time::{Duration, Instant};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;

struct Args {
    input: String,
    url: Option<String>,
    realtime: bool,
    loops: u32,
}

fn parse_args() -> Result<Args, String> {
    let mut args = Args {
        input: String::new(),
        url: None,
        realtime: false,
        loops: 1,
    };
    let mut it = std::env::args().skip(1);
    while let Some(arg) = it.next() {
        match arg.as_str() {
            "--input" => args.input = it.next().ok_or("--input needs a path")?,
            "--url" => args.url = Some(it.next().ok_or("--url needs a value")?),
            "--realtime" => args.realtime = true,
            "--loop" => {
                args.loops = it
                    .next()
                    .ok_or("--loop needs a count")?
                    .parse()
                    .map_err(|_| "--loop needs a number")?;
            }
            other => return Err(format!("unknown argument {other}")),
        }
    }
    if args.input.is_empty() {
        return Err("--input <file.flv> is required".into());
    }
    Ok(args)
}

/// Builds the RTMP metadata from the file's `onMetaData` tag, with defaults for anything missing.
fn metadata_from(script: Option<&Tag<'_>>) -> Metadata {
    let value = script.and_then(|tag| parse_script_tag(tag.body).ok().map(|(_, v)| v));
    let num = |name: &str, default: f64| {
        value
            .as_ref()
            .and_then(|v| v.get(name))
            .and_then(|v| v.as_f64())
            .unwrap_or(default)
    };
    Metadata {
        width: num("width", 1280.0) as u32,
        height: num("height", 720.0) as u32,
        framerate: num("framerate", 30.0),
        video_kbps: num("videodatarate", 2000.0),
        audio_kbps: num("audiodatarate", 128.0),
        audio_sample_rate: num("audiosamplerate", 48_000.0) as u32,
        stereo: value
            .as_ref()
            .and_then(|v| v.get("stereo"))
            .and_then(|v| v.as_bool())
            .unwrap_or(true),
        encoder: "prostor-spike".into(),
    }
}

struct Link {
    stream: TcpStream,
    started: Instant,
    bytes_sent: u64,
    bytes_received: u64,
    publishing_at: Option<Duration>,
}

impl Link {
    fn log(&self, message: &str) {
        eprintln!(
            "[{:>8.1} ms] {message}",
            self.started.elapsed().as_secs_f64() * 1000.0
        );
    }

    async fn apply(&mut self, outputs: Vec<Output>) -> Result<(), Box<dyn std::error::Error>> {
        for output in outputs {
            match output {
                Output::Send { bytes, .. } => {
                    self.stream.write_all(&bytes).await?;
                    self.bytes_sent += bytes.len() as u64;
                }
                Output::Event(event) => {
                    if event == Event::Publishing {
                        self.publishing_at = Some(self.started.elapsed());
                    }
                    self.log(&format!("event: {event:?}"));
                    if let Event::Rejected { code, description } = event {
                        return Err(format!("rejected by server: {code:?} ({description})").into());
                    }
                }
            }
        }
        Ok(())
    }

    /// Drains whatever the server sent without blocking (acks, status messages).
    async fn poll_input(
        &mut self,
        publisher: &mut Publisher,
    ) -> Result<(), Box<dyn std::error::Error>> {
        let mut buf = [0u8; 16 * 1024];
        loop {
            match self.stream.try_read(&mut buf) {
                Ok(0) => return Err("server closed the connection".into()),
                Ok(n) => {
                    self.bytes_received += n as u64;
                    let mut outputs = Vec::new();
                    publisher.handle_input(&buf[..n], &mut outputs)?;
                    self.apply(outputs).await?;
                }
                Err(e) if e.kind() == ErrorKind::WouldBlock => return Ok(()),
                Err(e) => return Err(e.into()),
            }
        }
    }
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args = parse_args().map_err(|e| {
        format!(
            "{e}\nusage: rtmp-publish --input <file.flv> [--url <rtmp url>] [--realtime] [--loop N]"
        )
    })?;
    let url = args
        .url
        .or_else(|| std::env::var("PROSTOR_SPIKE_RTMP_URL").ok())
        .ok_or("no target: pass --url or set PROSTOR_SPIKE_RTMP_URL")?;
    let target = Target::parse(&url)?;
    if target.tls {
        return Err(
            "rtmps:// is not implemented in this spike (TLS wrapper is a Phase 0 task)".into(),
        );
    }

    let data = std::fs::read(&args.input)?;
    let (header, mut pos) = Header::parse(&data)?.ok_or("truncated FLV header")?;
    let mut tags = Vec::new();
    while let Some((tag, n)) = parse_tag(&data[pos..])? {
        tags.push(tag);
        pos += n;
    }
    let count = |kind: TagKind| tags.iter().filter(|t| t.kind == kind).count();
    let duration_ms = tags.iter().map(|t| t.timestamp_ms).max().unwrap_or(0);
    eprintln!(
        "input: {} ({} bytes, audio={} video={}): {} video, {} audio, {} script tags, {} ms",
        args.input,
        data.len(),
        header.has_audio,
        header.has_video,
        count(TagKind::Video),
        count(TagKind::Audio),
        count(TagKind::Script),
        duration_ms
    );
    eprintln!(
        "target: {target} (realtime={}, loops={})",
        args.realtime, args.loops
    );

    let started = Instant::now();
    let stream = TcpStream::connect((target.host.as_str(), target.port)).await?;
    stream.set_nodelay(true)?;
    let mut link = Link {
        stream,
        started,
        bytes_sent: 0,
        bytes_received: 0,
        publishing_at: None,
    };
    link.log("tcp connected");

    let mut outputs = Vec::new();
    let mut publisher = Publisher::new(target, &mut outputs)?;
    link.apply(outputs).await?;

    let mut buf = vec![0u8; 16 * 1024];
    while publisher.state() != State::Publishing {
        let n = link.stream.read(&mut buf).await?;
        if n == 0 {
            return Err("server closed the connection during setup".into());
        }
        link.bytes_received += n as u64;
        let mut outputs = Vec::new();
        publisher.handle_input(&buf[..n], &mut outputs)?;
        link.apply(outputs).await?;
    }
    let setup_ms = link.started.elapsed().as_secs_f64() * 1000.0;

    let script = tags.iter().find(|t| t.kind == TagKind::Script);
    let mut outputs = Vec::new();
    publisher.publish_metadata(&metadata_from(script), &mut outputs)?;
    link.apply(outputs).await?;

    let stream_started = Instant::now();
    let period = duration_ms + 34;
    let mut sent_tags = 0u64;
    for pass in 0..args.loops {
        let offset = pass * period;
        for tag in &tags {
            if tag.kind == TagKind::Script {
                continue;
            }
            let shifted = Tag {
                timestamp_ms: tag.timestamp_ms + offset,
                ..*tag
            };
            if args.realtime {
                let due = stream_started + Duration::from_millis(u64::from(shifted.timestamp_ms));
                tokio::time::sleep_until(due.into()).await;
            }
            let mut outputs = Vec::new();
            publisher.publish_tag(&shifted, &mut outputs)?;
            link.apply(outputs).await?;
            sent_tags += 1;
            link.poll_input(&mut publisher).await?;
        }
        link.log(&format!(
            "pass {} done, {} bytes sent so far",
            pass + 1,
            link.bytes_sent
        ));
    }

    let mut outputs = Vec::new();
    publisher.stop(&mut outputs)?;
    link.apply(outputs).await?;
    tokio::time::sleep(Duration::from_millis(200)).await;
    link.poll_input(&mut publisher).await.ok();
    link.stream.shutdown().await.ok();

    let total_ms = link.started.elapsed().as_secs_f64() * 1000.0;
    let streamed_ms = stream_started.elapsed().as_secs_f64() * 1000.0;
    eprintln!(
        "done: setup {setup_ms:.1} ms (publishing after {:?}), {sent_tags} tags in {streamed_ms:.1} ms, {} bytes sent ({:.2} Mbit/s), {} bytes received, total {total_ms:.1} ms",
        link.publishing_at
            .map(|d| format!("{:.1} ms", d.as_secs_f64() * 1000.0))
            .unwrap_or_default(),
        link.bytes_sent,
        link.bytes_sent as f64 * 8.0 / 1e6 / (streamed_ms / 1000.0),
        link.bytes_received
    );
    Ok(())
}
