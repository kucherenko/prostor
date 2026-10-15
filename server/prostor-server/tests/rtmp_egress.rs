//! `RtmpEgress` against an in-process RTMP server: a clean publish, reconnection after
//! the server drops the socket, definitive rejections, unreachable destinations and the
//! private-address policy.

mod support;

use std::time::Duration;

use prostor_model::{EgressPhase, StreamInfo};
use prostor_protocol::{
    flv::{TagBuf, TagClass, TagKind},
    rtmp::Target,
};
use prostor_server::egress::{
    ConnectPolicy, Delivery, Egress, EgressError, RtmpEgress, StreamHeaders, metadata_tag,
};
use support::{Options, RtmpTestServer, fixture_media};

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

/// The fixture split into the cached headers and the media that follows them.
fn headers_and_media() -> (StreamHeaders, Vec<TagBuf>) {
    let media: Vec<TagBuf> = fixture_media().into_iter().map(|(_, tag)| tag).collect();
    let video_header = media[0].clone();
    let audio_header = media[1].clone();
    assert_eq!(
        video_header.as_tag().class().unwrap(),
        TagClass::SequenceHeader
    );
    assert_eq!(
        audio_header.as_tag().class().unwrap(),
        TagClass::SequenceHeader
    );
    let headers = StreamHeaders {
        info: info(),
        metadata: metadata_tag(&info()).unwrap(),
        video_sequence_header: video_header,
        audio_sequence_header: Some(audio_header),
    };
    (headers, media[2..].to_vec())
}

fn fast_policy() -> ConnectPolicy {
    ConnectPolicy {
        allow_private: true,
        connect_timeout: Duration::from_secs(2),
        publish_timeout: Duration::from_secs(5),
        write_timeout: Duration::from_secs(2),
        reconnect_min: Duration::from_millis(50),
        reconnect_max: Duration::from_millis(200),
    }
}

#[tokio::test]
async fn publishes_the_preamble_and_every_tag() {
    let server = RtmpTestServer::start(Options::default()).await;
    let target = Target::parse(&server.url("live", "key-one")).unwrap();
    let mut egress = RtmpEgress::new("rtmp-1", target, fast_policy());
    let (headers, media) = headers_and_media();

    egress.start(&headers).await.unwrap();
    assert_eq!(egress.health().phase, EgressPhase::Live);
    for (i, tag) in media.iter().enumerate() {
        assert_eq!(
            egress.push(i as u32 + 1, tag).await.unwrap(),
            Delivery::Sent
        );
    }
    egress.stop().await.unwrap();
    assert_eq!(egress.health().phase, EgressPhase::Stopped);

    assert!(
        server
            .wait_for(Duration::from_secs(5), |r| r.finished == 1)
            .await,
        "deleteStream reached the server"
    );
    let recorded = server.recorded();
    assert_eq!(recorded.connections, 1);
    assert_eq!(
        recorded.publishes,
        vec![("live".to_owned(), "key-one".to_owned())]
    );
    assert_eq!(recorded.metadata_widths, vec![1280]);
    assert_eq!(recorded.tags.len(), 2 + media.len());
    // The two sequence headers first (timestamp 0), then the media with its timestamps.
    assert_eq!(recorded.tags[0].kind, TagKind::Video);
    assert_eq!(recorded.tags[0].data, headers.video_sequence_header.body);
    assert_eq!(recorded.tags[1].kind, TagKind::Audio);
    for (received, sent) in recorded.tags[2..].iter().zip(&media) {
        assert_eq!(received.data, sent.body);
        assert_eq!(received.timestamp, sent.timestamp_ms);
        assert_eq!(received.kind, sent.kind);
    }
}

#[tokio::test]
async fn reconnects_after_the_server_drops_the_socket() {
    let server = RtmpTestServer::start(Options {
        drop_after_tags: Some(20),
        ..Options::default()
    })
    .await;
    let target = Target::parse(&server.url("live", "key-two")).unwrap();
    let mut egress = RtmpEgress::new("rtmp-2", target, fast_policy());
    let (headers, media) = headers_and_media();
    egress.start(&headers).await.unwrap();

    // Two passes over the fixture so a second keyframe exists after the drop.
    let mut seq = 0;
    let mut dropped = 0;
    let mut sent = 0;
    for pass in 0..2u32 {
        for tag in &media {
            seq += 1;
            let mut tag = tag.clone();
            tag.timestamp_ms += pass * 1100;
            match egress.push(seq, &tag).await.unwrap() {
                Delivery::Sent => sent += 1,
                Delivery::Dropped => dropped += 1,
            }
            tokio::time::sleep(Duration::from_millis(3)).await;
        }
    }
    egress.stop().await.unwrap();

    let health = egress.health();
    assert_eq!(health.reconnects, 1, "{health:?}");
    assert!(dropped > 0 && sent > 30, "sent {sent}, dropped {dropped}");
    assert!(
        server
            .wait_for(Duration::from_secs(5), |r| r.connections == 2
                && r.finished == 1)
            .await
    );
    let recorded = server.recorded();
    assert_eq!(
        recorded.publishes.len(),
        2,
        "a second publish on the new connection"
    );
    assert_eq!(recorded.metadata_widths, vec![1280, 1280]);
    let second: Vec<_> = recorded.tags.iter().filter(|t| t.connection == 2).collect();
    assert!(second.len() > 10);
    assert_eq!(
        (second[0].kind, second[0].timestamp, &second[0].data),
        (
            TagKind::Video,
            0,
            &headers.video_sequence_header.body.to_vec()
        ),
        "the new connection starts with the sequence headers"
    );
    assert_eq!(second[1].kind, TagKind::Audio);
    let first_video = second[2..]
        .iter()
        .find(|t| t.kind == TagKind::Video)
        .expect("video after the reconnect");
    assert_eq!(first_video.data[0] >> 4, 1, "video resumes at a keyframe");
    let first_media = &second[2];
    assert_eq!(
        first_media.timestamp, 0,
        "timestamps restart at 0 on the new connection"
    );
}

#[tokio::test]
async fn a_refused_key_ends_the_egress_without_retrying() {
    let server = RtmpTestServer::start(Options {
        reject_publish: Some("NetStream.Publish.BadName"),
        ..Options::default()
    })
    .await;
    let target = Target::parse(&server.url("live", "wrong-key")).unwrap();
    let mut egress = RtmpEgress::new("rtmp-3", target, fast_policy());
    let (headers, media) = headers_and_media();
    let error = egress.start(&headers).await.unwrap_err();
    assert!(matches!(error, EgressError::Rejected(_)), "{error}");
    assert!(!error.to_string().contains("wrong-key"), "{error}");
    let health = egress.health();
    assert_eq!(health.phase, EgressPhase::Failed);
    assert!(
        health
            .detail
            .as_deref()
            .unwrap_or("")
            .contains("PublishBadName"),
        "{health:?}"
    );
    assert!(matches!(
        egress.push(1, &media[0]).await,
        Err(EgressError::Closed)
    ));
    tokio::time::sleep(Duration::from_millis(150)).await;
    assert_eq!(server.recorded().connections, 1, "no retry after a refusal");
}

#[tokio::test]
async fn an_unreachable_destination_keeps_retrying_with_backoff() {
    // A listener that is closed again: the port is known to be unreachable.
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    drop(listener);
    let target = Target::parse(&format!("rtmp://{addr}/live/key")).unwrap();
    let mut egress = RtmpEgress::new("rtmp-4", target, fast_policy());
    let (headers, media) = headers_and_media();
    egress.start(&headers).await.unwrap();
    let health = egress.health();
    assert_eq!(health.phase, EgressPhase::Reconnecting);
    assert!(
        health.detail.as_deref().unwrap_or("").contains("connect"),
        "{health:?}"
    );
    assert_eq!(egress.push(1, &media[0]).await.unwrap(), Delivery::Dropped);
    tokio::time::sleep(Duration::from_millis(80)).await;
    assert_eq!(
        egress.push(2, &media[1]).await.unwrap(),
        Delivery::Dropped,
        "a due attempt fails again and the tag is dropped"
    );
    assert_eq!(egress.health().reconnects, 0);
    egress.stop().await.unwrap();
}

#[tokio::test]
async fn private_destinations_are_refused_unless_allowed() {
    let policy = ConnectPolicy {
        allow_private: false,
        ..fast_policy()
    };
    let (headers, _) = headers_and_media();
    for url in [
        "rtmp://127.0.0.1/live/key",
        "rtmp://10.0.0.1/live/key",
        "rtmp://[::1]/live/key",
        "rtmps://169.254.1.1/live/key",
    ] {
        let target = Target::parse(url).unwrap();
        let mut egress = RtmpEgress::new("rtmp-5", target, policy);
        let error = egress.start(&headers).await.unwrap_err();
        assert!(matches!(error, EgressError::Rejected(_)), "{url}: {error}");
        assert!(error.to_string().contains("private"), "{url}: {error}");
        assert_eq!(egress.health().phase, EgressPhase::Failed);
    }
}
