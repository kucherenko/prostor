//! Runs in headless Chrome: `wasm-pack test --headless --chrome core/prostor-wasm`.

#![cfg(target_arch = "wasm32")]

use js_sys::{ArrayBuffer, DataView, Uint8Array};
use prostor_model::{LayerKind, RenderPlan, Scene};
use prostor_protocol::flv::{AvcPacketType, Header, MAX_BODY_LEN, TagKind, parse_exact, parse_tag};
use prostor_wasm::BufferSource;
use wasm_bindgen::JsCast;
use wasm_bindgen_test::{wasm_bindgen_test, wasm_bindgen_test_configure};

wasm_bindgen_test_configure!(run_in_browser);

fn typed_array(bytes: &[u8]) -> BufferSource {
    Uint8Array::from(bytes).unchecked_into()
}

fn array_buffer(bytes: &[u8]) -> BufferSource {
    Uint8Array::from(bytes).buffer().unchecked_into()
}

/// A `DataView` over the middle of a larger buffer: offset and length must be honoured.
fn data_view(bytes: &[u8]) -> BufferSource {
    let mut padded = vec![0xEE; 3];
    padded.extend_from_slice(bytes);
    padded.extend_from_slice(&[0xEE; 2]);
    let buffer: ArrayBuffer = Uint8Array::from(&padded[..]).buffer();
    DataView::new(&buffer, 3, bytes.len()).unchecked_into()
}

#[wasm_bindgen_test]
fn sequence_headers_accept_every_buffer_source_shape_and_refuse_empty_ones() {
    let config = [1u8, 0x4d, 0, 0x28, 0xff, 0xe1, 0, 1, 0x67];
    let mut expected = None;
    for (name, source) in [
        ("typed array", typed_array(&config)),
        ("ArrayBuffer", array_buffer(&config)),
        ("DataView", data_view(&config)),
    ] {
        let mut muxer = prostor_wasm::FlvMuxer::new(true, true);
        let tag = muxer.video_sequence_header(0.0, &source).unwrap().to_vec();
        let parsed = parse_exact(&tag).unwrap();
        assert_eq!(parsed.video().unwrap().data, &config, "{name}");
        let bytes = expected.get_or_insert(tag.clone());
        assert_eq!(*bytes, tag, "{name}: identical output");
        let audio = muxer
            .audio_sequence_header(0.0, &array_buffer(&[0x11, 0x90]))
            .unwrap()
            .to_vec();
        assert_eq!(
            parse_exact(&audio).unwrap().audio().unwrap().data,
            &[0x11, 0x90]
        );
    }

    let mut muxer = prostor_wasm::FlvMuxer::new(true, true);
    assert!(muxer.video_sequence_header(0.0, &typed_array(&[])).is_err());
    assert!(
        muxer
            .video_sequence_header(0.0, &array_buffer(&[]))
            .is_err()
    );
    assert!(muxer.audio_sequence_header(0.0, &typed_array(&[])).is_err());
    let not_a_buffer: BufferSource = wasm_bindgen::JsValue::from_str("bytes").unchecked_into();
    assert!(muxer.video_sequence_header(0.0, &not_a_buffer).is_err());
    assert_eq!(
        muxer.origin(),
        None,
        "refused input does not touch the clock"
    );
    assert_eq!(muxer.tags_written(), 0.0);
}

#[wasm_bindgen_test]
fn compensated_audio_slightly_before_the_origin_lands_at_zero() {
    let mut muxer = prostor_wasm::FlvMuxer::new(true, true);
    muxer.set_origin(10_000.0).unwrap();
    let early = muxer.audio(10_000.0 - 44.0, &[1]).unwrap().to_vec();
    assert_eq!(parse_exact(&early).unwrap().timestamp_ms, 0);
    let later = muxer.audio(10_000.0 + 21.3, &[1]).unwrap().to_vec();
    assert_eq!(parse_exact(&later).unwrap().timestamp_ms, 21);
    assert!(
        muxer.audio(10_000.0 - 1000.0, &[1]).is_err(),
        "a second before the origin is a clock error"
    );
}

#[wasm_bindgen_test]
fn hello_scene_round_trips_through_js() {
    let value = prostor_wasm::hello_scene().expect("hello_scene serializes");
    let scene: Scene = serde_wasm_bindgen::from_value(value).expect("deserializes back");
    assert_eq!(scene, prostor_wasm::sample_scene());
    assert_eq!(scene.layers.len(), 2);
    assert!(matches!(scene.layers[1].kind, LayerKind::Text { .. }));
}

#[wasm_bindgen_test]
fn version_matches_the_crate() {
    assert_eq!(prostor_wasm::version(), env!("CARGO_PKG_VERSION"));
}

#[wasm_bindgen_test]
fn render_plan_orders_the_sample_scene_for_the_program() {
    let scene = prostor_wasm::hello_scene().unwrap();
    let plan = prostor_wasm::render_plan(scene, 1280, 720).expect("plan serializes");
    let plan: RenderPlan = serde_wasm_bindgen::from_value(plan).expect("deserializes back");
    assert_eq!((plan.width, plan.height), (1280, 720));
    assert_eq!(plan.layers.len(), 2);
    assert!(matches!(plan.layers[0].kind, LayerKind::Camera));
    assert!(matches!(plan.layers[1].kind, LayerKind::Text { .. }));
    assert_eq!(
        (plan.layers[0].rect.w, plan.layers[0].rect.h),
        (1280.0, 720.0),
        "the full-frame camera is clamped to the 720p program"
    );
    assert!(plan.layers.iter().all(|l| l.opacity == 1.0));

    let err = prostor_wasm::render_plan(wasm_bindgen::JsValue::from_str("nope"), 1, 1);
    assert!(err.is_err(), "a non-scene value is an error, not a panic");
}

#[wasm_bindgen_test]
fn flv_muxer_produces_parseable_tags_with_rebased_timestamps() {
    let mut muxer = prostor_wasm::FlvMuxer::new(true, true);
    let header = muxer.header().to_vec();
    assert_eq!(
        Header::parse(&header).unwrap(),
        Some((
            Header {
                has_audio: true,
                has_video: true
            },
            13
        ))
    );

    let metadata = muxer
        .metadata(1920, 1080, 30.0, 6000.0, 128.0, 48_000, true)
        .unwrap()
        .to_vec();
    assert_eq!(parse_exact(&metadata).unwrap().kind, TagKind::Script);

    // The encoder clock started at 1 000 000 ms; the stream must start at 0.
    muxer.set_origin(1_000_000.0).unwrap();
    assert_eq!(muxer.origin(), Some(1_000_000.0));
    let sequence = muxer
        .video_sequence_header(1_000_000.0, &typed_array(&[1, 0x64, 0, 0x28, 0xff, 0xe1]))
        .unwrap()
        .to_vec();
    let (tag, _) = parse_tag(&sequence).unwrap().unwrap();
    assert_eq!(tag.timestamp_ms, 0);
    assert_eq!(
        tag.video().unwrap().packet_type,
        AvcPacketType::SequenceHeader
    );

    let frame = muxer
        .video(1_000_033.4, true, 0.0, &[0, 0, 0, 2, 0x65, 0x88])
        .unwrap()
        .to_vec();
    let tag = parse_exact(&frame).unwrap();
    assert_eq!(tag.timestamp_ms, 33);
    assert_eq!(tag.video().unwrap().data, &[0, 0, 0, 2, 0x65, 0x88]);

    let audio = muxer.audio(1_000_021.0, &[0xDE, 0xAD]).unwrap().to_vec();
    let tag = parse_exact(&audio).unwrap();
    assert_eq!(tag.timestamp_ms, 21);
    assert_eq!(tag.audio().unwrap().data, &[0xDE, 0xAD]);

    assert_eq!(muxer.tags_written(), 4.0);
    assert_eq!(
        muxer.bytes_written() as usize,
        header.len() + metadata.len() + sequence.len() + frame.len() + audio.len()
    );

    muxer.rebase();
    assert_eq!(muxer.origin(), None);
    let after = muxer.audio(5_000_000.0, &[1]).unwrap().to_vec();
    assert_eq!(parse_exact(&after).unwrap().timestamp_ms, 0);
}

#[wasm_bindgen_test]
fn flv_muxer_rejects_bad_timestamps_and_oversized_bodies() {
    let mut muxer = prostor_wasm::FlvMuxer::new(true, true);
    assert!(muxer.audio(f64::NAN, &[1]).is_err());
    assert!(muxer.audio(f64::INFINITY, &[1]).is_err());
    assert_eq!(
        muxer.origin(),
        None,
        "rejected timestamps do not set the origin"
    );
    assert!(muxer.audio(10.0, &[1]).is_ok());
    assert!(muxer.audio(-500.0, &[1]).is_err(), "well before the origin");
    assert!(
        muxer.audio(4_294_967_296.0 + 10.0, &[1]).is_err(),
        "2^32 ms after the origin"
    );
    assert!(muxer.video(20.0, true, f64::NAN, &[1]).is_err());
    assert!(
        muxer.video(20.0, true, 1e9, &[1]).is_err(),
        "composition time out of range"
    );
    let big = vec![0u8; MAX_BODY_LEN];
    assert!(
        muxer.video(20.0, true, 0.0, &big).is_err(),
        "body too large"
    );
    assert_eq!(muxer.tags_written(), 1.0, "only the valid tag was written");
    assert!(
        muxer.audio(30.0, &[1]).is_ok(),
        "errors do not poison the muxer"
    );
}
