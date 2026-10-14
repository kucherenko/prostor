//! FLV conformance and byte-identity tests that run natively (`cargo test`) and in headless
//! Chrome (`wasm-pack test --headless --chrome core/prostor-protocol`).
//!
//! The fixture is the first second of a file written by ffmpeg 8.1 (libx264 main profile
//! with B-frames, AAC-LC 48 kHz stereo). See `docs/spikes/0002-*.md` and `0003-*.md`.

use prostor_protocol::flv::{
    AacPacketType, AvcPacketType, Demuxer, FrameType, Header, Metadata, MuxConfig, Muxer, TagClass,
    TagKind, parse_exact, parse_script_tag, parse_tag,
};
use sha2::{Digest, Sha256};

#[cfg(target_arch = "wasm32")]
use wasm_bindgen_test::{wasm_bindgen_test, wasm_bindgen_test_configure};

#[cfg(target_arch = "wasm32")]
wasm_bindgen_test_configure!(run_in_browser);

const FIXTURE: &[u8] = include_bytes!("fixtures/testsrc-720p-1s.flv");

/// SHA-256 of `remux_fixture()`, computed natively. The wasm run must reproduce it.
const EXPECTED_SHA256: &str = "be46d2b376ba2034ad43a35c7f9bd82fc7600d5cf6b18e2733771bdee7d56fe3";

fn sha256_hex(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

struct Parsed<'a> {
    header: Header,
    /// `(tag, raw bytes including trailer)`
    tags: Vec<(prostor_protocol::flv::Tag<'a>, &'a [u8])>,
}

fn parse_all(input: &[u8]) -> Parsed<'_> {
    let (header, mut pos) = Header::parse(input)
        .expect("valid header")
        .expect("complete header");
    let mut tags = Vec::new();
    while pos < input.len() {
        let (tag, n) = parse_tag(&input[pos..])
            .expect("valid tag")
            .expect("complete tag");
        tags.push((tag, &input[pos..pos + n]));
        pos += n;
    }
    assert_eq!(pos, input.len(), "trailing garbage");
    Parsed { header, tags }
}

fn metadata() -> Metadata {
    Metadata {
        width: 1280,
        height: 720,
        framerate: 30.0,
        video_kbps: 2000.0,
        audio_kbps: 128.0,
        audio_sample_rate: 48_000,
        stereo: true,
        encoder: "prostor".into(),
    }
}

/// Re-muxes every audio/video tag of the fixture with our muxer, behind our own header and
/// metadata. Deterministic: pure byte manipulation and IEEE-754 encoding.
fn remux_fixture() -> Vec<u8> {
    let parsed = parse_all(FIXTURE);
    let mut muxer = Muxer::new(MuxConfig {
        video: parsed.header.has_video,
        audio: parsed.header.has_audio,
    });
    let mut out = Vec::with_capacity(FIXTURE.len());
    muxer.write_header(&mut out);
    muxer.write_metadata(&metadata(), &mut out).unwrap();
    for (tag, _) in &parsed.tags {
        remux_tag(&mut muxer, tag, &mut out);
    }
    out
}

fn remux_tag(muxer: &mut Muxer, tag: &prostor_protocol::flv::Tag<'_>, out: &mut Vec<u8>) {
    match tag.kind {
        TagKind::Video => {
            let video = tag.video().unwrap();
            match video.packet_type {
                AvcPacketType::SequenceHeader => {
                    muxer.write_video_sequence_header(tag.timestamp_ms, video.data, out)
                }
                AvcPacketType::Nalu => muxer.write_video(
                    tag.timestamp_ms,
                    video.frame_type == FrameType::Key,
                    video.composition_time_ms,
                    video.data,
                    out,
                ),
                AvcPacketType::EndOfSequence => {
                    muxer.write_video_end_of_sequence(tag.timestamp_ms, out)
                }
            }
            .unwrap();
        }
        TagKind::Audio => {
            let audio = tag.audio().unwrap();
            match audio.packet_type {
                AacPacketType::SequenceHeader => {
                    muxer.write_audio_sequence_header(tag.timestamp_ms, audio.data, out)
                }
                AacPacketType::Raw => muxer.write_audio(tag.timestamp_ms, audio.data, out),
            }
            .unwrap();
        }
        TagKind::Script => {}
    }
}

#[cfg_attr(not(target_arch = "wasm32"), test)]
#[cfg_attr(target_arch = "wasm32", wasm_bindgen_test)]
fn fixture_parses_as_ffmpeg_wrote_it() {
    let parsed = parse_all(FIXTURE);
    assert_eq!(
        parsed.header,
        Header {
            has_audio: true,
            has_video: true
        }
    );
    let kinds = |k: TagKind| parsed.tags.iter().filter(|(t, _)| t.kind == k).count();
    assert_eq!(kinds(TagKind::Script), 1);
    assert_eq!(
        kinds(TagKind::Video),
        34,
        "sequence header + 32 frames + end of sequence"
    );
    assert_eq!(
        kinds(TagKind::Audio),
        48,
        "sequence header + 47 frames of 1024 samples"
    );

    let (script, _) = parsed.tags[0];
    assert_eq!(script.kind, TagKind::Script);
    let (name, meta) = parse_script_tag(script.body).unwrap();
    assert_eq!(name, "onMetaData");
    assert_eq!(meta.get("width").and_then(|v| v.as_f64()), Some(1280.0));
    assert_eq!(meta.get("videocodecid").and_then(|v| v.as_f64()), Some(7.0));
    assert_eq!(
        meta.get("audiocodecid").and_then(|v| v.as_f64()),
        Some(10.0)
    );
    assert_eq!(
        meta.get("encoder").and_then(|v| v.as_str()),
        Some("Lavf62.12.100")
    );

    let video_header = parsed
        .tags
        .iter()
        .find(|(t, _)| t.kind == TagKind::Video)
        .unwrap()
        .0
        .video()
        .unwrap();
    assert_eq!(video_header.packet_type, AvcPacketType::SequenceHeader);
    assert_eq!(
        video_header.data[0], 1,
        "AVCDecoderConfigurationRecord version"
    );
    assert_eq!(
        &video_header.data[1..4],
        &[0x4d, 0x40, 0x1f],
        "main profile, level 3.1"
    );

    let audio_header = parsed
        .tags
        .iter()
        .find(|(t, _)| t.kind == TagKind::Audio)
        .unwrap()
        .0
        .audio()
        .unwrap();
    assert_eq!(audio_header.packet_type, AacPacketType::SequenceHeader);
    assert_eq!(
        &audio_header.data[..2],
        &[0x11, 0x90],
        "AudioSpecificConfig: AAC-LC, 48 kHz, stereo"
    );
    assert_eq!(
        audio_header.data.len(),
        5,
        "ffmpeg appends the SBR sync extension (0x56 0xe5 0x00)"
    );

    let b_frames = parsed
        .tags
        .iter()
        .filter(|(t, _)| t.kind == TagKind::Video)
        .filter_map(|(t, _)| t.video().ok())
        .filter(|v| v.packet_type == AvcPacketType::Nalu && v.composition_time_ms > 0)
        .count();
    assert!(b_frames > 0, "fixture has non-zero composition times");

    // Every tag classifies, and the classes are what a forwarding policy expects.
    let classes: Vec<TagClass> = parsed
        .tags
        .iter()
        .map(|(t, _)| t.class().unwrap())
        .collect();
    assert_eq!(classes[0], TagClass::Script);
    assert_eq!(
        classes
            .iter()
            .filter(|c| **c == TagClass::SequenceHeader)
            .count(),
        2
    );
    assert_eq!(classes.iter().filter(|c| **c == TagClass::Key).count(), 1);
    assert_eq!(
        classes.iter().filter(|c| **c == TagClass::Delta).count(),
        31
    );
    assert_eq!(
        classes
            .iter()
            .filter(|c| **c == TagClass::EndOfSequence)
            .count(),
        1
    );
    assert_eq!(
        classes.iter().filter(|c| **c == TagClass::Audio).count(),
        47
    );
}

#[cfg_attr(not(target_arch = "wasm32"), test)]
#[cfg_attr(target_arch = "wasm32", wasm_bindgen_test)]
fn remuxed_audio_and_video_tags_are_byte_identical_to_ffmpeg() {
    let parsed = parse_all(FIXTURE);
    let mut muxer = Muxer::new(MuxConfig {
        video: true,
        audio: true,
    });
    let mut compared = 0;
    for (tag, raw) in &parsed.tags {
        if tag.kind == TagKind::Script {
            continue;
        }
        let mut out = Vec::new();
        remux_tag(&mut muxer, tag, &mut out);
        assert_eq!(
            out.as_slice(),
            *raw,
            "{:?} tag at {} ms",
            tag.kind,
            tag.timestamp_ms
        );
        assert_eq!(
            parse_exact(&out).unwrap(),
            *tag,
            "one tag per message framing"
        );
        compared += 1;
    }
    assert_eq!(compared, 82, "every audio and video tag");
}

#[cfg_attr(not(target_arch = "wasm32"), test)]
#[cfg_attr(target_arch = "wasm32", wasm_bindgen_test)]
fn remuxed_fixture_hashes_the_same_in_every_runtime() {
    let out = remux_fixture();
    let reparsed = parse_all(&out);
    assert_eq!(reparsed.tags.len(), 83, "our metadata + 82 media tags");
    assert_eq!(
        sha256_hex(&out),
        EXPECTED_SHA256,
        "output of {} bytes",
        out.len()
    );
}

#[cfg_attr(not(target_arch = "wasm32"), test)]
#[cfg_attr(target_arch = "wasm32", wasm_bindgen_test)]
fn incremental_reader_recovers_every_tag_of_the_fixture() {
    let mut demuxer = Demuxer::new();
    let mut tags = Vec::new();
    for chunk in FIXTURE.chunks(1500) {
        demuxer.feed(chunk).unwrap();
        tags.extend(demuxer.drain_tags().unwrap());
    }
    assert_eq!(tags.len(), 83);
    assert_eq!(demuxer.pending(), 0);
    assert_eq!(demuxer.error(), None);
    let parsed = parse_all(FIXTURE);
    for (owned, (borrowed, _)) in tags.iter().zip(&parsed.tags) {
        assert_eq!(owned.as_tag(), *borrowed);
    }
}
