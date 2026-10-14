//! RTMP publishing session (native only), built on `rml_rtmp`.
//!
//! Sans-IO: the caller owns the TCP socket (and TLS for `rtmps://`), feeds every byte it
//! receives into [`Publisher::handle_input`] and writes every [`Output::Send`] to the
//! socket in order. Audio and video go in as FLV tag bodies, exactly as the browser's
//! muxer produced them; RTMP messages and FLV tag bodies share the same layout.
//!
//! The stream key is a [`Secret`]: it is redacted in every `Debug`/`Display`, errors
//! never echo the URL they failed on, and text sent by the server is scrubbed and capped
//! before it becomes an [`Event`].
//!
//! Failure is atomic: a protocol error moves the session to [`State::Failed`] and every
//! later call returns [`Error::Failed`]; precondition errors ([`Error::NotPublishing`])
//! leave the session untouched.

use crate::flv::{self, TagClass, TagKind};
use crate::secret::Secret;
use bytes::Bytes;
use rml_rtmp::handshake::{Handshake, HandshakeError, HandshakeProcessResult, PeerType};
use rml_rtmp::rml_amf0::Amf0Value;
use rml_rtmp::sessions::{
    ClientSession, ClientSessionConfig, ClientSessionError, ClientSessionEvent,
    ClientSessionResult, PublishRequestType, StreamMetadata,
};
use rml_rtmp::time::RtmpTimestamp;
use std::fmt;

/// Longest host name accepted (DNS limit).
pub const MAX_HOST_LEN: usize = 253;
/// Longest application path accepted.
pub const MAX_APP_LEN: usize = 256;
/// Longest stream key accepted.
pub const MAX_KEY_LEN: usize = 1024;
/// Longest server-provided text kept in an [`Event`].
pub const MAX_DESCRIPTION_LEN: usize = 256;

/// Why a URL was rejected. Deliberately carries no part of the input.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UrlProblem {
    /// Not `rtmp://` or `rtmps://`.
    Scheme,
    /// Empty host, credentials in the authority, or malformed IPv6 brackets.
    Host,
    /// Port is not a number in `1..=65535`.
    Port,
    /// No application path.
    Path,
    /// No stream key segment.
    MissingKey,
    /// Host, application or key longer than the caps.
    TooLong,
}

/// Where to publish, parsed from `rtmp://host[:port]/app[/more]/stream-key`.
#[derive(Clone)]
pub struct Target {
    pub host: String,
    pub port: u16,
    pub tls: bool,
    pub app: String,
    pub stream_key: Secret,
}

impl Target {
    /// Parses an ingest URL. The last path segment is the stream key, everything before
    /// it is the application name (Twitch: `app`, YouTube: `live2`).
    pub fn parse(url: &str) -> Result<Self, Error> {
        let (tls, host, port, path) = parse_parts(url)?;
        let (app, key) = path
            .rsplit_once('/')
            .ok_or(Error::InvalidUrl(UrlProblem::MissingKey))?;
        Target::build(tls, host, port, app, key)
    }

    /// Builds a target from a key-less URL (`rtmp://host[:port]/app`, as stored in the
    /// board document) and the key kept separately.
    pub fn new(url_without_key: &str, stream_key: &str) -> Result<Self, Error> {
        let (tls, host, port, app) = parse_parts(url_without_key)?;
        Target::build(tls, host, port, app, stream_key)
    }

    fn build(tls: bool, host: &str, port: u16, app: &str, key: &str) -> Result<Self, Error> {
        if app.is_empty() {
            return Err(Error::InvalidUrl(UrlProblem::Path));
        }
        if key.is_empty() {
            return Err(Error::InvalidUrl(UrlProblem::MissingKey));
        }
        if host.len() > MAX_HOST_LEN || app.len() > MAX_APP_LEN || key.len() > MAX_KEY_LEN {
            return Err(Error::InvalidUrl(UrlProblem::TooLong));
        }
        Ok(Target {
            host: host.to_owned(),
            port,
            tls,
            app: app.to_owned(),
            stream_key: Secret::new(key),
        })
    }

    /// The `tcUrl` sent in the `connect` command; some servers require it.
    pub fn tc_url(&self) -> String {
        let scheme = if self.tls { "rtmps" } else { "rtmp" };
        if self.host.contains(':') {
            format!("{scheme}://[{}]:{}/{}", self.host, self.port, self.app)
        } else {
            format!("{scheme}://{}:{}/{}", self.host, self.port, self.app)
        }
    }
}

/// Splits `scheme://authority/path` into `(tls, host, port, path)`; the path has no
/// leading or trailing slashes.
fn parse_parts(url: &str) -> Result<(bool, &str, u16, &str), Error> {
    let (scheme, rest) = url
        .split_once("://")
        .ok_or(Error::InvalidUrl(UrlProblem::Scheme))?;
    let tls = match scheme {
        "rtmp" => false,
        "rtmps" => true,
        _ => return Err(Error::InvalidUrl(UrlProblem::Scheme)),
    };
    let (authority, path) = rest.split_once('/').unwrap_or((rest, ""));
    if authority.contains('@') {
        return Err(Error::InvalidUrl(UrlProblem::Host));
    }
    let default_port = if tls { 443 } else { 1935 };
    let (host, port) = if let Some(bracketed) = authority.strip_prefix('[') {
        let (host, after) = bracketed
            .split_once(']')
            .ok_or(Error::InvalidUrl(UrlProblem::Host))?;
        match after.strip_prefix(':') {
            None if after.is_empty() => (host, default_port),
            None => return Err(Error::InvalidUrl(UrlProblem::Host)),
            Some(port) => (host, parse_port(port)?),
        }
    } else {
        match authority.rsplit_once(':') {
            Some((host, port)) => (host, parse_port(port)?),
            None => (authority, default_port),
        }
    };
    if host.is_empty() || host.len() > MAX_HOST_LEN {
        return Err(Error::InvalidUrl(if host.is_empty() {
            UrlProblem::Host
        } else {
            UrlProblem::TooLong
        }));
    }
    let path = path.trim_matches('/');
    let path = path.split_once('?').map_or(path, |(p, _)| p);
    Ok((tls, host, port, path))
}

fn parse_port(port: &str) -> Result<u16, Error> {
    match port.parse::<u16>() {
        Ok(0) | Err(_) => Err(Error::InvalidUrl(UrlProblem::Port)),
        Ok(port) => Ok(port),
    }
}

impl fmt::Debug for Target {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Target")
            .field("host", &self.host)
            .field("port", &self.port)
            .field("tls", &self.tls)
            .field("app", &self.app)
            .field("stream_key", &self.stream_key)
            .finish()
    }
}

impl fmt::Display for Target {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}/{}", self.tc_url(), self.stream_key)
    }
}

/// Something the caller must do.
#[derive(PartialEq)]
pub enum Output {
    /// Write these bytes to the socket, in order. `can_be_dropped` is the chunk-level
    /// flag from the RTMP serializer; it is only ever set for delta video frames.
    /// GOP-aware dropping belongs before [`Publisher::publish_tag`], based on
    /// [`flv::Tag::class`].
    Send {
        bytes: Vec<u8>,
        can_be_dropped: bool,
    },
    /// A state change worth reporting.
    Event(Event),
}

impl fmt::Debug for Output {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Output::Send {
                bytes,
                can_be_dropped,
            } => f
                .debug_struct("Send")
                .field("len", &bytes.len())
                .field("can_be_dropped", can_be_dropped)
                .finish(),
            Output::Event(event) => f.debug_tuple("Event").field(event).finish(),
        }
    }
}

/// Session events.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Event {
    HandshakeCompleted,
    Connected,
    Publishing,
    /// The server refused the connection or the publish; the session is now failed.
    /// `description` is server text, scrubbed of the stream key and capped.
    Rejected {
        code: RejectCode,
        description: String,
    },
    /// An `onStatus` code this session does not act on (scrubbed and capped).
    Status {
        code: String,
    },
    AcknowledgementReceived {
        bytes: u32,
    },
    /// Something the server sent that a publisher does not need.
    Unhandled(Unhandled),
}

/// Why the server refused us.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RejectCode {
    ConnectRejected,
    ConnectFailed,
    PublishBadName,
    PublishDenied,
    PublishFailed,
    PublishRejected,
    /// An `_error` result or error-level status with a code not listed above.
    Other,
}

impl RejectCode {
    /// Maps a status or error code string.
    fn from_code(code: &str) -> Option<Self> {
        Some(match code {
            "NetConnection.Connect.Rejected" => RejectCode::ConnectRejected,
            "NetConnection.Connect.Failed" => RejectCode::ConnectFailed,
            "NetStream.Publish.BadName" => RejectCode::PublishBadName,
            "NetStream.Publish.Denied" => RejectCode::PublishDenied,
            "NetStream.Publish.Failed" => RejectCode::PublishFailed,
            "NetStream.Publish.Rejected" => RejectCode::PublishRejected,
            _ => return None,
        })
    }
}

/// Server messages a publisher ignores. Variant names only: the payloads are
/// server-controlled and never copied into events.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Unhandled {
    Amf0Command,
    TransactionResult,
    StreamMetadata,
    VideoData,
    AudioData,
    PlaybackAccepted,
    PingResponse,
    /// A raw RTMP message type the session does not know (`SetPeerBandwidth` is 6).
    Message {
        type_id: u8,
    },
}

/// Where the session is in its life cycle.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum State {
    Handshaking,
    Connecting,
    RequestingStream,
    Publishing,
    Stopped,
    Failed,
}

/// Session error.
#[derive(Debug)]
pub enum Error {
    InvalidUrl(UrlProblem),
    Handshake(HandshakeError),
    Session(ClientSessionError),
    Flv(flv::Error),
    /// The operation needs the `Publishing` state. The session is unchanged.
    NotPublishing(State),
    /// The session failed earlier (protocol error or rejection) and accepts no more input.
    Failed,
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::InvalidUrl(problem) => write!(f, "invalid RTMP url ({problem:?})"),
            Error::Handshake(e) => write!(f, "RTMP handshake failed: {e}"),
            Error::Session(e) => write!(f, "RTMP session error: {e}"),
            Error::Flv(e) => write!(f, "invalid FLV tag: {e}"),
            Error::NotPublishing(state) => write!(f, "not publishing (state {state:?})"),
            Error::Failed => write!(f, "session failed"),
        }
    }
}

impl std::error::Error for Error {}

impl From<HandshakeError> for Error {
    fn from(e: HandshakeError) -> Self {
        Error::Handshake(e)
    }
}

impl From<ClientSessionError> for Error {
    fn from(e: ClientSessionError) -> Self {
        Error::Session(e)
    }
}

impl From<flv::Error> for Error {
    fn from(e: flv::Error) -> Self {
        Error::Flv(e)
    }
}

/// A publishing session: handshake, `connect`, `createStream`, `publish`, then data.
/// Every method appends what the caller must do to `out`.
pub struct Publisher {
    target: Target,
    state: State,
    handshake: Option<Handshake>,
    session: Option<ClientSession>,
}

impl Publisher {
    /// Starts a session; appends the first handshake bytes (C0 + C1).
    pub fn new(target: Target, out: &mut Vec<Output>) -> Result<Self, Error> {
        let mut handshake = Handshake::new(PeerType::Client);
        let c0c1 = handshake.generate_outbound_p0_and_p1()?;
        push_send(out, c0c1);
        Ok(Publisher {
            target,
            state: State::Handshaking,
            handshake: Some(handshake),
            session: None,
        })
    }

    /// Current state.
    pub fn state(&self) -> State {
        self.state
    }

    /// The target, with its key redacted in `Debug`/`Display`.
    pub fn target(&self) -> &Target {
        &self.target
    }

    /// Feeds bytes received from the server.
    pub fn handle_input(&mut self, bytes: &[u8], out: &mut Vec<Output>) -> Result<(), Error> {
        self.fallible(|this| match this.state {
            State::Handshaking => this.handshake_input(bytes, out),
            _ => this.session_input(bytes, out),
        })
    }

    /// Sends `@setDataFrame onMetaData`. Requires the `Publishing` state.
    pub fn publish_metadata(
        &mut self,
        metadata: &flv::Metadata,
        out: &mut Vec<Output>,
    ) -> Result<(), Error> {
        self.fallible(|this| {
            let session = this.publishing_session()?;
            let mut stream_metadata = StreamMetadata::new();
            stream_metadata.video_width = Some(metadata.width);
            stream_metadata.video_height = Some(metadata.height);
            stream_metadata.video_codec_id = Some(u32::from(flv::CODEC_ID_AVC));
            stream_metadata.video_frame_rate = Some(metadata.framerate as f32);
            stream_metadata.video_bitrate_kbps = Some(metadata.video_kbps as u32);
            stream_metadata.audio_codec_id = Some(u32::from(flv::SOUND_FORMAT_AAC));
            stream_metadata.audio_bitrate_kbps = Some(metadata.audio_kbps as u32);
            stream_metadata.audio_sample_rate = Some(metadata.audio_sample_rate);
            stream_metadata.audio_channels = Some(if metadata.stereo { 2 } else { 1 });
            stream_metadata.audio_is_stereo = Some(metadata.stereo);
            stream_metadata.encoder = Some(metadata.encoder.clone());
            let result = session.publish_metadata(&stream_metadata)?;
            this.absorb(vec![result], out)
        })
    }

    /// Sends one FLV audio or video tag body as an RTMP message. Script tags are ignored
    /// (metadata goes through [`Publisher::publish_metadata`]). Requires `Publishing`.
    /// Only delta video frames are marked droppable; audio and keyframes never are.
    pub fn publish_tag(&mut self, tag: &flv::Tag<'_>, out: &mut Vec<Output>) -> Result<(), Error> {
        self.fallible(|this| {
            if this.state != State::Publishing {
                return Err(Error::NotPublishing(this.state));
            }
            let class = tag.class()?;
            let session = this.publishing_session()?;
            let timestamp = RtmpTimestamp::new(tag.timestamp_ms);
            let data = Bytes::copy_from_slice(tag.body);
            let result = match tag.kind {
                TagKind::Video => {
                    session.publish_video_data(data, timestamp, class == TagClass::Delta)?
                }
                TagKind::Audio => session.publish_audio_data(data, timestamp, false)?,
                TagKind::Script => return Ok(()),
            };
            this.absorb(vec![result], out)
        })
    }

    /// Ends publishing (`deleteStream`). The caller closes the socket afterwards.
    pub fn stop(&mut self, out: &mut Vec<Output>) -> Result<(), Error> {
        self.fallible(|this| {
            if let Some(session) = this.session.as_mut() {
                let results = session.stop_publishing()?;
                this.absorb(results, out)?;
            }
            this.state = State::Stopped;
            Ok(())
        })
    }

    /// Runs `f`; any error other than a precondition failure poisons the session.
    fn fallible(&mut self, f: impl FnOnce(&mut Self) -> Result<(), Error>) -> Result<(), Error> {
        if self.state == State::Failed {
            return Err(Error::Failed);
        }
        match f(self) {
            Ok(()) => Ok(()),
            Err(Error::NotPublishing(state)) => Err(Error::NotPublishing(state)),
            Err(e) => {
                self.fail();
                Err(e)
            }
        }
    }

    fn fail(&mut self) {
        self.state = State::Failed;
        self.handshake = None;
        self.session = None;
    }

    fn handshake_input(&mut self, bytes: &[u8], out: &mut Vec<Output>) -> Result<(), Error> {
        let handshake = self.handshake.as_mut().ok_or(Error::Failed)?;
        match handshake.process_bytes(bytes)? {
            HandshakeProcessResult::InProgress { response_bytes } => {
                push_send(out, response_bytes);
            }
            HandshakeProcessResult::Completed {
                response_bytes,
                remaining_bytes,
            } => {
                push_send(out, response_bytes);
                out.push(Output::Event(Event::HandshakeCompleted));
                self.start_session(out)?;
                self.handshake = None;
                if !remaining_bytes.is_empty() {
                    self.session_input(&remaining_bytes, out)?;
                }
            }
        }
        Ok(())
    }

    fn start_session(&mut self, out: &mut Vec<Output>) -> Result<(), Error> {
        let mut config = ClientSessionConfig::new();
        config.tc_url = Some(self.target.tc_url());
        config.flash_version = format!(
            "FMLE/3.0 (compatible; prostor/{})",
            env!("CARGO_PKG_VERSION")
        );
        let (mut session, results) = ClientSession::new(config)?;
        let connect = session.request_connection(self.target.app.clone())?;
        self.session = Some(session);
        self.state = State::Connecting;
        self.absorb(results, out)?;
        self.absorb(vec![connect], out)
    }

    fn session_input(&mut self, bytes: &[u8], out: &mut Vec<Output>) -> Result<(), Error> {
        let session = self.session.as_mut().ok_or(Error::Failed)?;
        let results = session.handle_input(bytes)?;
        self.absorb(results, out)
    }

    fn publishing_session(&mut self) -> Result<&mut ClientSession, Error> {
        if self.state != State::Publishing {
            return Err(Error::NotPublishing(self.state));
        }
        self.session.as_mut().ok_or(Error::Failed)
    }

    fn absorb(
        &mut self,
        results: Vec<ClientSessionResult>,
        out: &mut Vec<Output>,
    ) -> Result<(), Error> {
        for result in results {
            match result {
                ClientSessionResult::OutboundResponse(packet) => out.push(Output::Send {
                    bytes: packet.bytes,
                    can_be_dropped: packet.can_be_dropped,
                }),
                ClientSessionResult::RaisedEvent(event) => self.on_event(event, out)?,
                ClientSessionResult::UnhandleableMessageReceived(payload) => {
                    out.push(Output::Event(Event::Unhandled(Unhandled::Message {
                        type_id: payload.type_id,
                    })))
                }
            }
            if self.state == State::Failed {
                break;
            }
        }
        Ok(())
    }

    fn on_event(&mut self, event: ClientSessionEvent, out: &mut Vec<Output>) -> Result<(), Error> {
        match event {
            ClientSessionEvent::ConnectionRequestAccepted => {
                out.push(Output::Event(Event::Connected));
                let key = self.target.stream_key.expose_secret().to_owned();
                let session = self.session.as_mut().ok_or(Error::Failed)?;
                let result = session.request_publishing(key, PublishRequestType::Live)?;
                self.state = State::RequestingStream;
                self.absorb(vec![result], out)?;
            }
            ClientSessionEvent::ConnectionRequestRejected { description } => {
                self.reject(RejectCode::ConnectRejected, &description, out);
            }
            ClientSessionEvent::PublishRequestAccepted => {
                self.state = State::Publishing;
                out.push(Output::Event(Event::Publishing));
            }
            ClientSessionEvent::UnhandleableOnStatusCode { code } => {
                match RejectCode::from_code(&code) {
                    Some(reject) => self.reject(reject, &code, out),
                    None => out.push(Output::Event(Event::Status {
                        code: self.sanitize(&code),
                    })),
                }
            }
            ClientSessionEvent::UnknownTransactionResultReceived {
                additional_values, ..
            } => match error_info(&additional_values) {
                Some((code, description)) => self.reject(code, &description, out),
                None => out.push(Output::Event(Event::Unhandled(
                    Unhandled::TransactionResult,
                ))),
            },
            ClientSessionEvent::AcknowledgementReceived { bytes_received } => {
                out.push(Output::Event(Event::AcknowledgementReceived {
                    bytes: bytes_received,
                }));
            }
            ClientSessionEvent::UnhandleableAmf0Command { .. } => {
                out.push(Output::Event(Event::Unhandled(Unhandled::Amf0Command)))
            }
            ClientSessionEvent::StreamMetadataReceived { .. } => {
                out.push(Output::Event(Event::Unhandled(Unhandled::StreamMetadata)))
            }
            ClientSessionEvent::VideoDataReceived { .. } => {
                out.push(Output::Event(Event::Unhandled(Unhandled::VideoData)))
            }
            ClientSessionEvent::AudioDataReceived { .. } => {
                out.push(Output::Event(Event::Unhandled(Unhandled::AudioData)))
            }
            ClientSessionEvent::PlaybackRequestAccepted => {
                out.push(Output::Event(Event::Unhandled(Unhandled::PlaybackAccepted)))
            }
            ClientSessionEvent::PingResponseReceived { .. } => {
                out.push(Output::Event(Event::Unhandled(Unhandled::PingResponse)))
            }
        }
        Ok(())
    }

    /// Records a rejection: the session is failed and the event carries scrubbed text.
    fn reject(&mut self, code: RejectCode, description: &str, out: &mut Vec<Output>) {
        let description = self.sanitize(description);
        self.fail();
        out.push(Output::Event(Event::Rejected { code, description }));
    }

    /// Server text never reaches an event verbatim: the key is scrubbed and the length capped.
    fn sanitize(&self, text: &str) -> String {
        let scrubbed = self.target.stream_key.scrub(text);
        match scrubbed.char_indices().nth(MAX_DESCRIPTION_LEN) {
            Some((cut, _)) => scrubbed[..cut].to_owned(),
            None => scrubbed,
        }
    }
}

/// Reads `(code, description)` from the information object of an `_error` result or an
/// error-level status; `None` for anything that is not an error.
fn error_info(values: &[Amf0Value]) -> Option<(RejectCode, String)> {
    let Some(Amf0Value::Object(info)) = values.first() else {
        return None;
    };
    let text = |name: &str| match info.get(name) {
        Some(Amf0Value::Utf8String(s)) => Some(s.as_str()),
        _ => None,
    };
    let code = text("code");
    let is_error = matches!(text("level"), Some("error" | "_error"));
    let reject = match code.and_then(RejectCode::from_code) {
        Some(reject) => reject,
        None if is_error => RejectCode::Other,
        None => return None,
    };
    let description = text("description").or(code).unwrap_or_default().to_owned();
    Some((reject, description))
}

fn push_send(out: &mut Vec<Output>, bytes: Vec<u8>) {
    if !bytes.is_empty() {
        out.push(Output::Send {
            bytes,
            can_be_dropped: false,
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rml_rtmp::sessions::{
        ServerSession, ServerSessionConfig, ServerSessionEvent, ServerSessionResult,
    };

    const PLACEHOLDER_KEY: &str = "stream-token-placeholder-0123";

    #[test]
    fn parses_platform_urls() {
        let twitch = Target::parse("rtmp://live.twitch.tv/app/live_123_abc").unwrap();
        assert_eq!(
            (twitch.host.as_str(), twitch.port, twitch.tls),
            ("live.twitch.tv", 1935, false)
        );
        assert_eq!(
            (twitch.app.as_str(), twitch.stream_key.expose_secret()),
            ("app", "live_123_abc")
        );
        assert_eq!(twitch.tc_url(), "rtmp://live.twitch.tv:1935/app");

        let youtube = Target::parse("rtmps://a.rtmps.youtube.com:443/live2/abcd-efgh").unwrap();
        assert_eq!(
            (youtube.port, youtube.tls, youtube.app.as_str()),
            (443, true, "live2")
        );

        let nested = Target::parse("rtmp://127.0.0.1:1935/live/room/key").unwrap();
        assert_eq!(
            (nested.app.as_str(), nested.stream_key.expose_secret()),
            ("live/room", "key")
        );

        let trailing = Target::parse("rtmp://127.0.0.1/live/key/").unwrap();
        assert_eq!(trailing.stream_key.expose_secret(), "key");

        let query = Target::parse("rtmp://127.0.0.1/live/key?x=1").unwrap();
        assert_eq!(query.stream_key.expose_secret(), "key");
    }

    #[test]
    fn parses_ipv6_hosts() {
        let v6 = Target::parse("rtmp://[::1]:2935/live/key").unwrap();
        assert_eq!((v6.host.as_str(), v6.port), ("::1", 2935));
        assert_eq!(v6.tc_url(), "rtmp://[::1]:2935/live");
        let default_port = Target::parse("rtmps://[2001:db8::7]/live/key").unwrap();
        assert_eq!(
            (default_port.host.as_str(), default_port.port),
            ("2001:db8::7", 443)
        );
        assert_eq!(
            Target::parse("rtmp://[::1/live/key")
                .unwrap_err()
                .to_string(),
            "invalid RTMP url (Host)"
        );
        assert!(matches!(
            Target::parse("rtmp://[::1]x/live/key"),
            Err(Error::InvalidUrl(UrlProblem::Host))
        ));
    }

    #[test]
    fn builds_from_a_keyless_url_and_a_separate_key() {
        let target = Target::new("rtmp://a.rtmp.youtube.com/live2", "abcd-efgh").unwrap();
        assert_eq!(target.app, "live2");
        assert_eq!(target.stream_key.expose_secret(), "abcd-efgh");
        let nested = Target::new("rtmp://host:1935/live/room/", "k").unwrap();
        assert_eq!(nested.app, "live/room");
        assert!(matches!(
            Target::new("rtmp://host/live2", ""),
            Err(Error::InvalidUrl(UrlProblem::MissingKey))
        ));
        assert!(matches!(
            Target::new("rtmp://host", "k"),
            Err(Error::InvalidUrl(UrlProblem::Path))
        ));
    }

    #[test]
    fn caps_host_app_and_key_lengths() {
        let long_app = format!("rtmp://host/{}/key", "a".repeat(MAX_APP_LEN + 1));
        assert!(matches!(
            Target::parse(&long_app),
            Err(Error::InvalidUrl(UrlProblem::TooLong))
        ));
        let long_key = format!("rtmp://host/app/{}", "k".repeat(MAX_KEY_LEN + 1));
        assert!(matches!(
            Target::parse(&long_key),
            Err(Error::InvalidUrl(UrlProblem::TooLong))
        ));
        let long_host = format!("rtmp://{}/app/key", "h".repeat(MAX_HOST_LEN + 1));
        assert!(matches!(
            Target::parse(&long_host),
            Err(Error::InvalidUrl(UrlProblem::TooLong))
        ));
        let ok = format!(
            "rtmp://host/{}/{}",
            "a".repeat(MAX_APP_LEN),
            "k".repeat(MAX_KEY_LEN)
        );
        assert!(Target::parse(&ok).is_ok());
    }

    #[test]
    fn malformed_urls_fail_without_echoing_any_part_of_the_input() {
        let markers = [
            "MARKER-HOST",
            "MARKER-APP",
            "MARKER-KEY",
            "MARKER-USER",
            "MARKER-PASS",
        ];
        let urls = [
            "http://MARKER-HOST/MARKER-APP/MARKER-KEY",
            "rtmp://MARKER-HOST",
            "rtmp://MARKER-HOST/MARKER-APP",
            "rtmp://MARKER-HOST/MARKER-APP/",
            "rtmp://MARKER-HOST:xx/MARKER-APP/MARKER-KEY",
            "rtmp://MARKER-HOST:0/MARKER-APP/MARKER-KEY",
            "rtmp://MARKER-HOST:99999/MARKER-APP/MARKER-KEY/",
            "rtmp:///MARKER-APP/MARKER-KEY",
            "rtmp://MARKER-USER:MARKER-PASS@MARKER-HOST/MARKER-APP/MARKER-KEY",
            "rtmps://[MARKER-HOST/MARKER-APP/MARKER-KEY",
            "MARKER-HOST/MARKER-APP/MARKER-KEY",
        ];
        for url in urls {
            let err = Target::parse(url).unwrap_err();
            assert!(matches!(err, Error::InvalidUrl(_)), "{url}");
            for text in [err.to_string(), format!("{err:?}")] {
                for marker in markers {
                    assert!(!text.contains(marker), "{url}: {text}");
                }
            }
        }
    }

    #[test]
    fn never_prints_the_stream_key() {
        let target =
            Target::parse(&format!("rtmp://live.twitch.tv/app/{PLACEHOLDER_KEY}")).unwrap();
        for text in [format!("{target}"), format!("{target:?}")] {
            assert!(!text.contains(PLACEHOLDER_KEY), "{text}");
            assert!(text.contains("redacted"), "{text}");
        }
    }

    #[test]
    fn starts_with_the_handshake_and_refuses_data_before_publishing() {
        let target = Target::parse("rtmp://127.0.0.1/live/key").unwrap();
        let mut out = Vec::new();
        let mut publisher = Publisher::new(target, &mut out).unwrap();
        assert_eq!(publisher.state(), State::Handshaking);
        assert_eq!(out.len(), 1);
        match &out[0] {
            Output::Send {
                bytes,
                can_be_dropped,
            } => {
                assert_eq!(bytes.len(), 1 + 1536, "C0 + C1");
                assert_eq!(bytes[0], 3, "plain RTMP version byte");
                assert!(!can_be_dropped);
            }
            other => panic!("unexpected {other:?}"),
        }
        let tag = flv::Tag {
            kind: TagKind::Audio,
            timestamp_ms: 0,
            body: &[0xAF, 1, 0],
        };
        let mut out = Vec::new();
        assert!(matches!(
            publisher.publish_tag(&tag, &mut out),
            Err(Error::NotPublishing(State::Handshaking))
        ));
        assert_eq!(
            publisher.state(),
            State::Handshaking,
            "precondition errors do not fail the session"
        );
    }

    #[test]
    fn protocol_errors_fail_the_session_atomically() {
        let target = Target::parse("rtmp://127.0.0.1/live/key").unwrap();
        let mut out = Vec::new();
        let mut publisher = Publisher::new(target, &mut out).unwrap();
        let mut bad_s0 = vec![6u8];
        bad_s0.extend_from_slice(&[0; 1536]);
        assert!(matches!(
            publisher.handle_input(&bad_s0, &mut out),
            Err(Error::Handshake(HandshakeError::BadVersionId))
        ));
        assert_eq!(publisher.state(), State::Failed);
        assert!(matches!(
            publisher.handle_input(&[0], &mut out),
            Err(Error::Failed)
        ));
        assert!(matches!(publisher.stop(&mut out), Err(Error::Failed)));
        let tag = flv::Tag {
            kind: TagKind::Audio,
            timestamp_ms: 0,
            body: &[0xAF, 1, 0],
        };
        assert!(matches!(
            publisher.publish_tag(&tag, &mut out),
            Err(Error::Failed)
        ));
    }

    /// Policy for the in-process server.
    #[derive(Clone, Copy)]
    enum ServerPolicy {
        Accept,
        RejectConnect,
        RejectPublish,
    }

    /// An in-process RTMP server (handshake + `rml_rtmp::ServerSession`) wired back to
    /// back with a [`Publisher`] through byte buffers.
    struct Harness {
        publisher: Publisher,
        policy: ServerPolicy,
        handshake: Option<Handshake>,
        session: Option<ServerSession>,
        client_to_server: Vec<u8>,
        server_to_client: Vec<u8>,
        outputs: Vec<Output>,
        server_events: Vec<ServerSessionEvent>,
    }

    impl Harness {
        fn new(policy: ServerPolicy) -> Self {
            let target =
                Target::parse(&format!("rtmp://127.0.0.1/live/{PLACEHOLDER_KEY}")).unwrap();
            let mut outputs = Vec::new();
            let publisher = Publisher::new(target, &mut outputs).unwrap();
            let mut harness = Harness {
                publisher,
                policy,
                handshake: Some(Handshake::new(PeerType::Server)),
                session: None,
                client_to_server: Vec::new(),
                server_to_client: Vec::new(),
                outputs: Vec::new(),
                server_events: Vec::new(),
            };
            harness.take(outputs);
            harness
        }

        /// Moves publisher outputs onto the wire and keeps them for inspection.
        fn take(&mut self, outputs: Vec<Output>) {
            for output in &outputs {
                if let Output::Send { bytes, .. } = output {
                    self.client_to_server.extend_from_slice(bytes);
                }
            }
            self.outputs.extend(outputs);
        }

        fn server_results(&mut self, results: Vec<ServerSessionResult>) {
            for result in results {
                match result {
                    ServerSessionResult::OutboundResponse(packet) => {
                        self.server_to_client.extend_from_slice(&packet.bytes)
                    }
                    ServerSessionResult::RaisedEvent(event) => {
                        let session = self.session.as_mut().unwrap();
                        let more = match (&event, self.policy) {
                            (
                                ServerSessionEvent::ConnectionRequested { request_id, .. },
                                ServerPolicy::RejectConnect,
                            ) => session
                                .reject_request(
                                    *request_id,
                                    "NetConnection.Connect.Rejected",
                                    "no such application",
                                )
                                .unwrap(),
                            (ServerSessionEvent::ConnectionRequested { request_id, .. }, _) => {
                                session.accept_request(*request_id).unwrap()
                            }
                            (
                                ServerSessionEvent::PublishStreamRequested {
                                    request_id,
                                    stream_key,
                                    ..
                                },
                                ServerPolicy::RejectPublish,
                            ) => session
                                .reject_request(
                                    *request_id,
                                    "NetStream.Publish.BadName",
                                    &format!("stream {stream_key} is not allowed"),
                                )
                                .unwrap(),
                            (ServerSessionEvent::PublishStreamRequested { request_id, .. }, _) => {
                                session.accept_request(*request_id).unwrap()
                            }
                            _ => Vec::new(),
                        };
                        self.server_events.push(event);
                        self.server_results(more);
                    }
                    ServerSessionResult::UnhandleableMessageReceived(_) => {}
                }
            }
        }

        /// Runs both sides until nothing moves any more.
        fn pump(&mut self) -> Result<(), Error> {
            for _ in 0..50 {
                if self.client_to_server.is_empty() && self.server_to_client.is_empty() {
                    return Ok(());
                }
                let inbound = std::mem::take(&mut self.client_to_server);
                if !inbound.is_empty() {
                    match self.handshake.as_mut() {
                        Some(handshake) => match handshake.process_bytes(&inbound).unwrap() {
                            HandshakeProcessResult::InProgress { response_bytes } => {
                                self.server_to_client.extend_from_slice(&response_bytes)
                            }
                            HandshakeProcessResult::Completed {
                                response_bytes,
                                remaining_bytes,
                            } => {
                                self.server_to_client.extend_from_slice(&response_bytes);
                                self.handshake = None;
                                let (session, results) =
                                    ServerSession::new(ServerSessionConfig::new()).unwrap();
                                self.session = Some(session);
                                self.server_results(results);
                                if !remaining_bytes.is_empty() {
                                    let results = self
                                        .session
                                        .as_mut()
                                        .unwrap()
                                        .handle_input(&remaining_bytes)
                                        .unwrap();
                                    self.server_results(results);
                                }
                            }
                        },
                        None => {
                            let results = self
                                .session
                                .as_mut()
                                .unwrap()
                                .handle_input(&inbound)
                                .unwrap();
                            self.server_results(results);
                        }
                    }
                }
                let outbound = std::mem::take(&mut self.server_to_client);
                if !outbound.is_empty() {
                    let mut outputs = Vec::new();
                    let result = self.publisher.handle_input(&outbound, &mut outputs);
                    self.take(outputs);
                    result?;
                }
            }
            panic!("the sessions did not settle");
        }

        fn events(&self) -> Vec<&Event> {
            self.outputs
                .iter()
                .filter_map(|o| match o {
                    Output::Event(e) => Some(e),
                    Output::Send { .. } => None,
                })
                .collect()
        }
    }

    #[test]
    fn publishes_through_an_in_process_server() {
        let mut h = Harness::new(ServerPolicy::Accept);
        h.pump().unwrap();
        assert_eq!(h.publisher.state(), State::Publishing);
        let events = h.events();
        assert!(events.contains(&&Event::HandshakeCompleted));
        assert!(events.contains(&&Event::Connected));
        assert!(events.contains(&&Event::Publishing));
        assert!(matches!(
            h.server_events.iter().find(|e| matches!(e, ServerSessionEvent::PublishStreamRequested { .. })),
            Some(ServerSessionEvent::PublishStreamRequested { app_name, stream_key, .. })
                if app_name == "live" && stream_key == PLACEHOLDER_KEY
        ));

        let metadata = flv::Metadata {
            width: 1280,
            height: 720,
            framerate: 30.0,
            video_kbps: 2000.0,
            audio_kbps: 128.0,
            audio_sample_rate: 48_000,
            stereo: true,
            encoder: "prostor-test".into(),
        };
        let mut outputs = Vec::new();
        h.publisher
            .publish_metadata(&metadata, &mut outputs)
            .unwrap();
        let video = flv::Tag {
            kind: TagKind::Video,
            timestamp_ms: 40,
            body: &[0x17, 1, 0, 0, 0, 0, 0, 0, 2, 0x65, 0x88],
        };
        let audio = flv::Tag {
            kind: TagKind::Audio,
            timestamp_ms: 21,
            body: &[0xAF, 1, 0xDE, 0xAD],
        };
        h.publisher.publish_tag(&video, &mut outputs).unwrap();
        h.publisher.publish_tag(&audio, &mut outputs).unwrap();
        h.take(outputs);
        h.pump().unwrap();

        let received_video = h.server_events.iter().find_map(|e| match e {
            ServerSessionEvent::VideoDataReceived {
                data, timestamp, ..
            } => Some((data.clone(), timestamp.value)),
            _ => None,
        });
        assert_eq!(
            received_video,
            Some((Bytes::copy_from_slice(video.body), 40))
        );
        let received_audio = h.server_events.iter().find_map(|e| match e {
            ServerSessionEvent::AudioDataReceived {
                data, timestamp, ..
            } => Some((data.clone(), timestamp.value)),
            _ => None,
        });
        assert_eq!(
            received_audio,
            Some((Bytes::copy_from_slice(audio.body), 21))
        );
        assert!(h.server_events.iter().any(|e| matches!(
            e,
            ServerSessionEvent::StreamMetadataChanged { metadata, .. } if metadata.video_width == Some(1280)
        )));

        let debug = format!("{:?}", h.outputs);
        assert!(!debug.contains(PLACEHOLDER_KEY), "{debug}");
        let as_decimal_bytes: Vec<String> =
            PLACEHOLDER_KEY.bytes().map(|b| b.to_string()).collect();
        assert!(!debug.contains(&as_decimal_bytes.join(", ")), "{debug}");

        let mut outputs = Vec::new();
        h.publisher.stop(&mut outputs).unwrap();
        assert_eq!(h.publisher.state(), State::Stopped);
        h.take(outputs);
        h.pump().unwrap();
        assert!(
            h.server_events
                .iter()
                .any(|e| matches!(e, ServerSessionEvent::PublishStreamFinished { .. }))
        );
    }

    #[test]
    fn only_delta_video_frames_are_droppable() {
        let mut h = Harness::new(ServerPolicy::Accept);
        h.pump().unwrap();
        let cases: [(&str, TagKind, &[u8], bool); 7] = [
            (
                "avc sequence header",
                TagKind::Video,
                &[0x17, 0, 0, 0, 0, 1, 0x4d],
                false,
            ),
            ("keyframe", TagKind::Video, &[0x17, 1, 0, 0, 0, 9], false),
            (
                "generated keyframe",
                TagKind::Video,
                &[0x47, 1, 0, 0, 0, 9],
                false,
            ),
            ("inter frame", TagKind::Video, &[0x27, 1, 0, 0, 0, 9], true),
            (
                "disposable inter frame",
                TagKind::Video,
                &[0x37, 1, 0, 0, 0, 9],
                true,
            ),
            (
                "end of sequence",
                TagKind::Video,
                &[0x17, 2, 0, 0, 0],
                false,
            ),
            ("aac raw frame", TagKind::Audio, &[0xAF, 1, 9], false),
        ];
        for (name, kind, body, expected) in cases {
            let mut outputs = Vec::new();
            h.publisher
                .publish_tag(
                    &flv::Tag {
                        kind,
                        timestamp_ms: 1,
                        body,
                    },
                    &mut outputs,
                )
                .unwrap();
            let flags: Vec<bool> = outputs
                .iter()
                .filter_map(|o| match o {
                    Output::Send { can_be_dropped, .. } => Some(*can_be_dropped),
                    Output::Event(_) => None,
                })
                .collect();
            assert_eq!(flags, [expected], "{name}");
        }
        let mut outputs = Vec::new();
        assert!(matches!(
            h.publisher.publish_tag(
                &flv::Tag {
                    kind: TagKind::Video,
                    timestamp_ms: 1,
                    body: &[0x57, 1]
                },
                &mut outputs
            ),
            Err(Error::Flv(flv::Error::BadFrameType(5)))
        ));
        assert_eq!(
            h.publisher.state(),
            State::Failed,
            "malformed media poisons the session"
        );
    }

    #[test]
    fn a_rejected_publish_fails_the_session_with_a_scrubbed_description() {
        let mut h = Harness::new(ServerPolicy::RejectPublish);
        h.pump().unwrap();
        assert_eq!(h.publisher.state(), State::Failed);
        let rejected = h.events().into_iter().find_map(|e| match e {
            Event::Rejected { code, description } => Some((*code, description.clone())),
            _ => None,
        });
        let (code, description) = rejected.expect("a Rejected event");
        assert_eq!(code, RejectCode::PublishBadName);
        assert!(!description.contains(PLACEHOLDER_KEY), "{description}");
        assert!(description.contains("<redacted>"), "{description}");
        let mut outputs = Vec::new();
        assert!(matches!(
            h.publisher.handle_input(&[0], &mut outputs),
            Err(Error::Failed)
        ));
        let tag = flv::Tag {
            kind: TagKind::Audio,
            timestamp_ms: 0,
            body: &[0xAF, 1, 0],
        };
        assert!(matches!(
            h.publisher.publish_tag(&tag, &mut outputs),
            Err(Error::Failed)
        ));
    }

    #[test]
    fn a_rejected_connection_fails_the_session() {
        let mut h = Harness::new(ServerPolicy::RejectConnect);
        h.pump().unwrap();
        assert_eq!(h.publisher.state(), State::Failed);
        assert!(h.events().iter().any(|e| matches!(
            e,
            Event::Rejected { code: RejectCode::ConnectRejected, description } if description == "no such application"
        )));
        assert!(!h.events().contains(&&Event::Connected));
    }

    #[test]
    fn server_text_is_scrubbed_and_capped() {
        let target = Target::parse(&format!("rtmp://127.0.0.1/live/{PLACEHOLDER_KEY}")).unwrap();
        let mut out = Vec::new();
        let publisher = Publisher::new(target, &mut out).unwrap();
        let long = format!("{PLACEHOLDER_KEY} {}", "x".repeat(1000));
        let text = publisher.sanitize(&long);
        assert!(!text.contains(PLACEHOLDER_KEY));
        assert!(text.starts_with("<redacted> x"));
        assert_eq!(text.chars().count(), MAX_DESCRIPTION_LEN);
    }

    #[test]
    fn error_info_recognises_error_results() {
        use std::collections::HashMap;
        let object = |pairs: &[(&str, &str)]| {
            Amf0Value::Object(
                pairs
                    .iter()
                    .map(|(k, v)| ((*k).to_owned(), Amf0Value::Utf8String((*v).to_owned())))
                    .collect::<HashMap<_, _>>(),
            )
        };
        assert_eq!(
            error_info(&[object(&[
                ("code", "NetStream.Publish.Denied"),
                ("description", "nope")
            ])]),
            Some((RejectCode::PublishDenied, "nope".to_owned()))
        );
        assert_eq!(
            error_info(&[object(&[("level", "error"), ("code", "Custom.Code")])]),
            Some((RejectCode::Other, "Custom.Code".to_owned()))
        );
        assert_eq!(
            error_info(&[object(&[("level", "status"), ("code", "Custom.Code")])]),
            None
        );
        assert_eq!(error_info(&[Amf0Value::Null]), None);
        assert_eq!(error_info(&[]), None);
    }
}
