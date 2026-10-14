//! Fan-out of one session's tags to its egresses, with the per-egress queue policy of
//! ADR 0003 §6.
//!
//! The supervisor validates the tag order the ingest protocol promises (video sequence
//! header first, audio sequence header before the first audio frame), caches the
//! sequence headers, starts every egress with them on the first media frame, and then
//! forwards tags. Each egress runs in its own task behind a bounded queue
//! ([`QUEUE_CAPACITY`]); when the queue is full, video delta frames are dropped for that
//! egress until its next keyframe, while audio, keyframes and sequence headers always
//! wait for room. Drops are counted per egress and shown as [`EgressPhase::Degraded`].
//!
//! [`Supervisor::acked_seq`] is what the server acknowledges to the browser: the newest
//! sequence number every live egress has consumed.

use std::{sync::Arc, time::Duration};

use prostor_model::{EgressPhase, EgressState, StreamInfo};
use prostor_protocol::flv::{self, TagBuf, TagClass, TagKind};
use tokio::{
    sync::{mpsc, oneshot, watch},
    task::JoinHandle,
};

use super::{Egress, StreamHeaders, metadata_tag};
use crate::room::RoomId;

/// Tags queued per egress before delta frames are dropped: about two seconds of a
/// 30 fps video with audio at 47 frames per second, as ADR 0003 asks for.
pub const QUEUE_CAPACITY: usize = 192;

/// Why a tag was refused. Egress failures are not errors of `push`: they show up in
/// [`Supervisor::states`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SupervisorError {
    /// The tag could not be parsed or classified (close code 4400, `bad_message`).
    Flv(flv::Error),
    /// The tag violates the protocol's order rules (close code 4400, `protocol`).
    Protocol(&'static str),
}

impl std::fmt::Display for SupervisorError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SupervisorError::Flv(e) => write!(f, "invalid tag: {e}"),
            SupervisorError::Protocol(why) => f.write_str(why),
        }
    }
}

impl std::error::Error for SupervisorError {}

enum Command {
    Start(Arc<StreamHeaders>),
    Tag(u32, TagBuf),
    Stop(oneshot::Sender<()>),
}

struct Slot {
    tx: mpsc::Sender<Command>,
    /// Shared with the task, which updates it after every consumed tag.
    state: watch::Sender<EgressState>,
    task: JoinHandle<()>,
    dropped: u64,
    /// A delta frame was dropped; keep dropping until the next keyframe.
    dropping: bool,
}

impl Slot {
    fn snapshot(&self) -> EgressState {
        let mut state = self.state.borrow().clone();
        state.dropped = self.dropped;
        if state.state == EgressPhase::Live && self.dropping {
            state.state = EgressPhase::Degraded;
        }
        state
    }

    fn is_live(&self) -> bool {
        !matches!(
            self.state.borrow().state,
            EgressPhase::Failed | EgressPhase::Stopped
        )
    }
}

/// The egresses of one session.
pub struct Supervisor {
    room: RoomId,
    info: StreamInfo,
    metadata: TagBuf,
    video_sequence_header: Option<TagBuf>,
    audio_sequence_header: Option<TagBuf>,
    started: bool,
    last_seq: u32,
    tags: u64,
    bytes: u64,
    slots: Vec<Slot>,
}

impl Supervisor {
    /// A supervisor for a stream described by `info`, without egresses yet.
    pub fn new(room: RoomId, info: StreamInfo) -> Result<Self, flv::Error> {
        let metadata = metadata_tag(&info)?;
        Ok(Supervisor {
            room,
            info,
            metadata,
            video_sequence_header: None,
            audio_sequence_header: None,
            started: false,
            last_seq: 0,
            tags: 0,
            bytes: 0,
            slots: Vec::new(),
        })
    }

    /// Adds an egress. Must happen before the first `push`; the egress is started with
    /// the cached headers once the first media frame arrives.
    pub fn add(&mut self, egress: Box<dyn Egress>) {
        assert!(
            !self.started,
            "egresses are added before the first media frame"
        );
        let (tx, rx) = mpsc::channel(QUEUE_CAPACITY);
        let initial = EgressState {
            id: egress.id().to_owned(),
            kind: egress.kind(),
            state: EgressPhase::Starting,
            bytes: 0,
            tags: 0,
            dropped: 0,
            last_seq: 0,
        };
        let (state, _) = watch::channel(initial);
        let task = tokio::spawn(run(egress, rx, state.clone(), self.room.clone()));
        self.slots.push(Slot {
            tx,
            state,
            task,
            dropped: 0,
            dropping: false,
        });
    }

    /// The newest sequence number pushed so far.
    pub fn last_seq(&self) -> u32 {
        self.last_seq
    }

    /// Tags and bytes received so far (sequence headers included).
    pub fn received(&self) -> (u64, u64) {
        (self.tags, self.bytes)
    }

    /// Whether the egresses have been started (the first media frame has arrived).
    pub fn is_started(&self) -> bool {
        self.started
    }

    /// The newest sequence number every live egress has consumed; the last pushed one
    /// when no egress is live.
    pub fn acked_seq(&self) -> u32 {
        self.slots
            .iter()
            .filter(|slot| slot.is_live())
            .map(|slot| slot.state.borrow().last_seq)
            .min()
            .unwrap_or(self.last_seq)
    }

    /// Current state of every egress.
    pub fn states(&self) -> Vec<EgressState> {
        self.slots.iter().map(Slot::snapshot).collect()
    }

    /// Validates and forwards one tag. Waits when an egress queue is full, except for
    /// delta frames, which are dropped for that egress until its next keyframe.
    pub async fn push(&mut self, seq: u32, tag: TagBuf) -> Result<(), SupervisorError> {
        if seq <= self.last_seq {
            return Err(SupervisorError::Protocol("sequence numbers must increase"));
        }
        let class = tag.as_tag().class().map_err(SupervisorError::Flv)?;
        self.check_order(&tag, class)?;
        self.last_seq = seq;
        self.tags += 1;
        self.bytes += tag.as_tag().wire_len() as u64;

        if class == TagClass::SequenceHeader {
            match tag.kind {
                TagKind::Video => self.video_sequence_header = Some(tag.clone()),
                TagKind::Audio => self.audio_sequence_header = Some(tag.clone()),
                TagKind::Script => unreachable!("scripts are refused by check_order"),
            }
            if !self.started {
                // Cached only; every egress writes it from `StreamHeaders` on start.
                return Ok(());
            }
        } else if !self.started {
            self.start_egresses().await;
        }
        self.forward(seq, tag, class).await;
        Ok(())
    }

    fn check_order(&self, tag: &TagBuf, class: TagClass) -> Result<(), SupervisorError> {
        if class == TagClass::Script {
            return Err(SupervisorError::Protocol(
                "script tags are written by the server",
            ));
        }
        if self.video_sequence_header.is_none()
            && !(tag.kind == TagKind::Video && class == TagClass::SequenceHeader)
        {
            return Err(SupervisorError::Protocol(
                "the first tag must be the video sequence header",
            ));
        }
        if tag.kind == TagKind::Audio {
            if self.info.audio_codec.is_none() {
                return Err(SupervisorError::Protocol(
                    "audio tag in a video-only stream",
                ));
            }
            if class == TagClass::Audio && self.audio_sequence_header.is_none() {
                return Err(SupervisorError::Protocol(
                    "audio frame before the audio sequence header",
                ));
            }
        }
        Ok(())
    }

    async fn start_egresses(&mut self) {
        let headers = Arc::new(StreamHeaders {
            info: self.info.clone(),
            metadata: self.metadata.clone(),
            video_sequence_header: self
                .video_sequence_header
                .clone()
                .expect("checked by check_order"),
            audio_sequence_header: self.audio_sequence_header.clone(),
        });
        self.started = true;
        for slot in &mut self.slots {
            // Queues are empty at this point, so this never waits long.
            let _ = slot.tx.send(Command::Start(headers.clone())).await;
        }
    }

    async fn forward(&mut self, seq: u32, tag: TagBuf, class: TagClass) {
        for slot in &mut self.slots {
            if !slot.is_live() {
                continue;
            }
            match class {
                TagClass::Delta => {
                    if slot.dropping {
                        slot.dropped += 1;
                        continue;
                    }
                    match slot.tx.try_send(Command::Tag(seq, tag.clone())) {
                        Ok(()) => {}
                        Err(mpsc::error::TrySendError::Full(_)) => {
                            slot.dropped += 1;
                            slot.dropping = true;
                        }
                        Err(mpsc::error::TrySendError::Closed(_)) => {}
                    }
                }
                TagClass::Key
                | TagClass::Audio
                | TagClass::SequenceHeader
                | TagClass::EndOfSequence
                | TagClass::Script => {
                    if class == TagClass::Key {
                        slot.dropping = false;
                    }
                    // A full queue means waiting, never dropping, for these.
                    let _ = slot.tx.send(Command::Tag(seq, tag.clone())).await;
                }
            }
        }
    }

    /// Stops every egress and waits up to `timeout` for them to finalize; an egress that
    /// misses the deadline is aborted and reported as failed. Returns the final states.
    pub async fn stop(&mut self, timeout: Duration) -> Vec<EgressState> {
        let mut pending = Vec::new();
        for slot in &self.slots {
            let (done_tx, done_rx) = oneshot::channel();
            if slot.tx.send(Command::Stop(done_tx)).await.is_ok() {
                pending.push(done_rx);
            }
        }
        let all_done = async {
            for done in pending {
                let _ = done.await;
            }
        };
        if tokio::time::timeout(timeout, all_done).await.is_err() {
            for slot in &self.slots {
                if slot.is_live() {
                    tracing::warn!(room = %self.room, id = %slot.state.borrow().id,
                        "egress did not stop within {timeout:?}; aborting");
                    slot.task.abort();
                    slot.state.send_modify(|s| s.state = EgressPhase::Failed);
                }
            }
        }
        self.states()
    }
}

impl Drop for Supervisor {
    fn drop(&mut self) {
        // Dropping the senders ends every task's loop, which finalizes its egress.
        self.slots.clear();
    }
}

/// The task behind one egress: runs its methods in order and publishes its state.
async fn run(
    mut egress: Box<dyn Egress>,
    mut rx: mpsc::Receiver<Command>,
    state: watch::Sender<EgressState>,
    room: RoomId,
) {
    let id = egress.id().to_owned();
    let fail = |what: &str, error: &dyn std::fmt::Display| {
        tracing::warn!(%room, %id, %error, "egress failed to {what}");
        state.send_modify(|s| s.state = EgressPhase::Failed);
    };
    let mut started = false;
    loop {
        match rx.recv().await {
            Some(Command::Start(headers)) => match egress.start(&headers).await {
                Ok(()) => {
                    started = true;
                    state.send_modify(|s| s.state = EgressPhase::Live);
                }
                Err(e) => return fail("start", &e),
            },
            Some(Command::Tag(seq, tag)) => match egress.push(seq, &tag).await {
                Ok(()) => state.send_modify(|s| {
                    s.tags += 1;
                    s.bytes += tag.as_tag().wire_len() as u64;
                    s.last_seq = seq;
                }),
                Err(e) => return fail("write", &e),
            },
            Some(Command::Stop(done)) => {
                match egress.stop().await {
                    Ok(()) => state.send_modify(|s| s.state = EgressPhase::Stopped),
                    Err(e) => fail("stop", &e),
                }
                let _ = done.send(());
                return;
            }
            None => {
                // The supervisor went away without `stop` (the session task ended
                // abruptly): still finalize, so a recording is never left unflushed.
                if started {
                    match egress.stop().await {
                        Ok(()) => state.send_modify(|s| s.state = EgressPhase::Stopped),
                        Err(e) => fail("stop", &e),
                    }
                }
                return;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::egress::EgressError;
    use async_trait::async_trait;
    use prostor_model::EgressKind;
    use prostor_protocol::flv::{parse_exact, write_tag};
    use std::sync::Mutex;

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
        parse_exact(&bytes).unwrap().to_owned()
    }

    fn video_header() -> TagBuf {
        tag(TagKind::Video, 0, &[0x17, 0, 0, 0, 0, 1, 0x4d, 0x40, 0x1f])
    }

    fn audio_header() -> TagBuf {
        tag(TagKind::Audio, 0, &[0xAF, 0, 0x11, 0x90])
    }

    fn key(ts: u32) -> TagBuf {
        tag(TagKind::Video, ts, &[0x17, 1, 0, 0, 0, 0xAA])
    }

    fn delta(ts: u32) -> TagBuf {
        tag(TagKind::Video, ts, &[0x27, 1, 0, 0, 0, 0xBB])
    }

    fn audio(ts: u32) -> TagBuf {
        tag(TagKind::Audio, ts, &[0xAF, 1, 0xCC])
    }

    #[derive(Default)]
    struct Log {
        headers: Option<StreamHeaders>,
        tags: Vec<(u32, TagClass)>,
        stopped: bool,
    }

    /// Records everything it is given; `gate` can hold it in `push`.
    struct TestEgress {
        log: Arc<Mutex<Log>>,
        gate: watch::Receiver<bool>,
        fail_on_start: bool,
    }

    #[async_trait]
    impl Egress for TestEgress {
        fn id(&self) -> &str {
            "test"
        }

        fn kind(&self) -> EgressKind {
            EgressKind::File
        }

        async fn start(&mut self, headers: &StreamHeaders) -> Result<(), EgressError> {
            if self.fail_on_start {
                return Err(EgressError::Closed);
            }
            self.log.lock().unwrap().headers = Some(headers.clone());
            Ok(())
        }

        async fn push(&mut self, seq: u32, tag: &TagBuf) -> Result<(), EgressError> {
            let mut gate = self.gate.clone();
            gate.wait_for(|blocked| !*blocked).await.unwrap();
            self.log
                .lock()
                .unwrap()
                .tags
                .push((seq, tag.as_tag().class().unwrap()));
            Ok(())
        }

        async fn stop(&mut self) -> Result<(), EgressError> {
            self.log.lock().unwrap().stopped = true;
            Ok(())
        }
    }

    fn supervisor_with_test_egress(
        fail_on_start: bool,
    ) -> (Supervisor, Arc<Mutex<Log>>, watch::Sender<bool>) {
        let log = Arc::new(Mutex::new(Log::default()));
        let (gate_tx, gate_rx) = watch::channel(false);
        let mut supervisor = Supervisor::new(RoomId::parse("r").unwrap(), info()).unwrap();
        supervisor.add(Box::new(TestEgress {
            log: log.clone(),
            gate: gate_rx,
            fail_on_start,
        }));
        (supervisor, log, gate_tx)
    }

    #[tokio::test]
    async fn caches_headers_starts_on_the_first_frame_and_forwards_in_order() {
        let (mut sup, log, _gate) = supervisor_with_test_egress(false);
        assert_eq!(sup.states()[0].state, EgressPhase::Starting);
        sup.push(1, video_header()).await.unwrap();
        sup.push(2, audio_header()).await.unwrap();
        assert!(!sup.is_started());
        assert_eq!(sup.acked_seq(), 0, "nothing consumed yet");
        sup.push(3, key(0)).await.unwrap();
        sup.push(4, audio(0)).await.unwrap();
        sup.push(5, delta(33)).await.unwrap();
        let states = sup.stop(Duration::from_secs(1)).await;
        assert_eq!(states[0].state, EgressPhase::Stopped);
        assert_eq!(states[0].tags, 3);
        assert_eq!(states[0].last_seq, 5);
        assert_eq!(states[0].dropped, 0);
        assert_eq!(sup.received().0, 5);

        let log = log.lock().unwrap();
        let headers = log.headers.as_ref().unwrap();
        assert_eq!(headers.video_sequence_header, video_header());
        assert_eq!(headers.audio_sequence_header, Some(audio_header()));
        assert_eq!(headers.metadata.kind, TagKind::Script);
        assert_eq!(
            log.tags,
            vec![
                (3, TagClass::Key),
                (4, TagClass::Audio),
                (5, TagClass::Delta)
            ]
        );
        assert!(log.stopped);
    }

    #[tokio::test]
    async fn refuses_tags_out_of_protocol_order() {
        let (mut sup, _log, _gate) = supervisor_with_test_egress(false);
        assert_eq!(
            sup.push(1, key(0)).await.unwrap_err(),
            SupervisorError::Protocol("the first tag must be the video sequence header")
        );
        assert_eq!(
            sup.push(1, audio_header()).await.unwrap_err(),
            SupervisorError::Protocol("the first tag must be the video sequence header")
        );
        sup.push(1, video_header()).await.unwrap();
        assert_eq!(
            sup.push(2, audio(0)).await.unwrap_err(),
            SupervisorError::Protocol("audio frame before the audio sequence header")
        );
        assert_eq!(
            sup.push(1, audio_header()).await.unwrap_err(),
            SupervisorError::Protocol("sequence numbers must increase")
        );
        let script = tag(TagKind::Script, 0, &[0x02, 0, 1, b'x', 0x05]);
        assert_eq!(
            sup.push(2, script).await.unwrap_err(),
            SupervisorError::Protocol("script tags are written by the server")
        );
        let garbage = tag(TagKind::Video, 0, &[0xFF]);
        assert!(matches!(
            sup.push(2, garbage).await.unwrap_err(),
            SupervisorError::Flv(_)
        ));
        assert_eq!(
            sup.last_seq(),
            1,
            "refused tags do not advance the sequence"
        );
        sup.stop(Duration::from_secs(1)).await;
    }

    #[tokio::test]
    async fn video_only_streams_refuse_audio() {
        let mut video_only = info();
        video_only.audio_codec = None;
        let mut sup = Supervisor::new(RoomId::parse("r").unwrap(), video_only).unwrap();
        sup.push(1, video_header()).await.unwrap();
        assert_eq!(
            sup.push(2, audio_header()).await.unwrap_err(),
            SupervisorError::Protocol("audio tag in a video-only stream")
        );
        sup.push(2, key(0)).await.unwrap();
        assert_eq!(
            sup.acked_seq(),
            2,
            "no egress: everything counts as consumed"
        );
    }

    #[tokio::test]
    async fn a_full_queue_drops_deltas_until_the_next_keyframe_but_never_audio() {
        let (mut sup, log, gate) = supervisor_with_test_egress(false);
        sup.push(1, video_header()).await.unwrap();
        sup.push(2, audio_header()).await.unwrap();
        gate.send(true).unwrap();
        sup.push(3, key(0)).await.unwrap();
        tokio::task::yield_now().await;
        // The egress is stuck on the keyframe; a burst of deltas overflows the queue.
        let burst = QUEUE_CAPACITY as u32 + 50;
        for i in 0..burst {
            sup.push(4 + i, delta(33 * (i + 1))).await.unwrap();
        }
        let state = &sup.states()[0];
        // 192 queued (plus the keyframe, if the task has not taken it yet), the rest dropped.
        assert!((50..=51).contains(&state.dropped), "{}", state.dropped);
        assert_eq!(state.state, EgressPhase::Degraded);
        let dropped_in_burst = state.dropped;
        assert_eq!(sup.acked_seq(), 0, "nothing consumed while blocked");

        // Release the egress; the queue drains, but deltas stay dropped until a keyframe.
        gate.send(false).unwrap();
        let mut seq = 4 + burst;
        for i in 0..10 {
            sup.push(seq, audio(i)).await.unwrap();
            seq += 1;
            sup.push(seq, delta(i)).await.unwrap();
            seq += 1;
        }
        assert_eq!(sup.states()[0].dropped, dropped_in_burst + 10);
        sup.push(seq, key(1000)).await.unwrap();
        seq += 1;
        sup.push(seq, delta(1033)).await.unwrap();
        let key_seq = seq - 1;
        let states = sup.stop(Duration::from_secs(1)).await;

        let log = log.lock().unwrap();
        let audio_count = log
            .tags
            .iter()
            .filter(|(_, c)| *c == TagClass::Audio)
            .count();
        assert_eq!(audio_count, 10, "audio is never dropped");
        let keys: Vec<u32> = log
            .tags
            .iter()
            .filter(|(_, c)| *c == TagClass::Key)
            .map(|(s, _)| *s)
            .collect();
        assert_eq!(keys, vec![3, key_seq], "keyframes are never dropped");
        let after_key = log.tags.iter().position(|(s, _)| *s == key_seq).unwrap();
        assert_eq!(log.tags[after_key + 1], (seq, TagClass::Delta));
        let deltas_between = log
            .tags
            .iter()
            .filter(|(s, c)| *c == TagClass::Delta && *s > 4 + burst && *s < key_seq)
            .count();
        assert_eq!(
            deltas_between, 0,
            "no delta between the overflow and the keyframe"
        );
        assert_eq!(states[0].dropped, dropped_in_burst + 10);
        assert_eq!(states[0].state, EgressPhase::Stopped);
        assert_eq!(states[0].last_seq, seq);
        assert_eq!(
            sup.received().0,
            states[0].tags + states[0].dropped + 2,
            "received = consumed + dropped + 2 cached headers"
        );
    }

    #[tokio::test]
    async fn a_failed_egress_is_reported_and_skipped() {
        let (mut sup, log, _gate) = supervisor_with_test_egress(true);
        sup.push(1, video_header()).await.unwrap();
        sup.push(2, key(0)).await.unwrap();
        // The task fails on start; wait for the state to settle.
        let mut rx = sup.slots[0].state.subscribe();
        tokio::time::timeout(
            Duration::from_secs(1),
            rx.wait_for(|s| s.state == EgressPhase::Failed),
        )
        .await
        .expect("failed state")
        .unwrap();
        sup.push(3, delta(33)).await.unwrap();
        assert_eq!(sup.states()[0].state, EgressPhase::Failed);
        assert_eq!(
            sup.acked_seq(),
            3,
            "a failed egress does not hold acks back"
        );
        let states = sup.stop(Duration::from_secs(1)).await;
        assert_eq!(states[0].state, EgressPhase::Failed);
        assert!(!log.lock().unwrap().stopped);
    }

    #[tokio::test]
    async fn stop_aborts_an_egress_that_hangs() {
        let (mut sup, _log, gate) = supervisor_with_test_egress(false);
        sup.push(1, video_header()).await.unwrap();
        gate.send(true).unwrap();
        sup.push(2, key(0)).await.unwrap();
        let states = sup.stop(Duration::from_millis(100)).await;
        assert_eq!(states[0].state, EgressPhase::Failed, "aborted mid-write");
        tokio::task::yield_now().await;
        assert!(sup.slots[0].task.is_finished());
    }

    #[tokio::test]
    async fn dropping_the_supervisor_still_finalizes_a_started_egress() {
        let (mut sup, log, _gate) = supervisor_with_test_egress(false);
        sup.push(1, video_header()).await.unwrap();
        sup.push(2, key(0)).await.unwrap();
        drop(sup);
        for _ in 0..100 {
            if log.lock().unwrap().stopped {
                return;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
        panic!("egress was not finalized");
    }
}
