//! An RTMP(S) destination (ADR 0004): `tokio` TCP, `tokio-rustls` for `rtmps://`, the
//! sans-IO [`Publisher`] from `prostor-protocol` in between.
//!
//! Connect-time safety: the host is resolved first and every address must be public
//! unless [`ConnectPolicy::allow_private`] is set, so a producer cannot point the server's
//! egress at the server's own network. Connecting, the RTMP handshake up to `publish`,
//! and every write are bounded by timeouts.
//!
//! Resilience: a lost connection or a transient refusal puts the egress into
//! [`EgressPhase::Reconnecting`]; tags are dropped (never queued) until the next attempt,
//! attempts back off exponentially, and a re-established connection starts with the
//! cached `onMetaData` and sequence headers, rebases timestamps to 0 and waits for the
//! next keyframe before sending video. A definitive refusal (wrong key, unknown
//! application) ends the egress instead.
//!
//! The stream key is only ever held inside a [`Target`] (redacted `Debug`) and is never
//! part of any error or status text.

use std::{
    net::{IpAddr, Ipv4Addr, SocketAddr},
    panic::{AssertUnwindSafe, catch_unwind},
    sync::{Arc, OnceLock},
    time::{Duration, Instant},
};

use async_trait::async_trait;
use prostor_model::{EgressKind, EgressPhase};
use prostor_protocol::{
    flv::{Tag, TagBuf, TagClass},
    rtmp::{Event, Output, Publisher, RejectCode, State, Target},
};
use tokio::{
    io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt},
    net::TcpStream,
    time::timeout,
};
use tokio_rustls::{
    TlsConnector,
    rustls::{ClientConfig, RootCertStore, pki_types::ServerName},
};

use super::{Delivery, Egress, EgressError, Health, StreamHeaders, stream_metadata};

/// Most bytes accepted from a destination over one connection. Servers send a few
/// hundred bytes of commands and then an acknowledgement every few megabytes; anything
/// near this limit is not an RTMP server we want to keep parsing.
pub const INBOUND_LIMIT: usize = 1024 * 1024;
const READ_CHUNK: usize = 16 * 1024;

/// How an [`RtmpEgress`] connects and retries.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ConnectPolicy {
    /// Accept destinations that resolve to loopback, link-local or private addresses.
    pub allow_private: bool,
    /// TCP connect (and TLS handshake) deadline.
    pub connect_timeout: Duration,
    /// From the first byte to `publish` accepted.
    pub publish_timeout: Duration,
    /// Deadline for writing one tag; a destination that stalls longer is reconnected.
    pub write_timeout: Duration,
    /// First retry delay; doubles per failure up to `reconnect_max`.
    pub reconnect_min: Duration,
    pub reconnect_max: Duration,
}

impl Default for ConnectPolicy {
    fn default() -> Self {
        ConnectPolicy {
            allow_private: false,
            connect_timeout: Duration::from_secs(5),
            publish_timeout: Duration::from_secs(10),
            write_timeout: Duration::from_secs(10),
            reconnect_min: Duration::from_secs(1),
            reconnect_max: Duration::from_secs(30),
        }
    }
}

trait Socket: AsyncRead + AsyncWrite + Send + Unpin {}
impl<T: AsyncRead + AsyncWrite + Send + Unpin> Socket for T {}

struct Connection {
    socket: Box<dyn Socket>,
    publisher: Publisher,
    inbound: usize,
    read_buf: Vec<u8>,
}

/// Why a connection ended or could not be made.
#[derive(Debug)]
enum Failure {
    /// Worth retrying (network, timeout, server went away).
    Transient(String),
    /// Retrying would not help (refused key or application, forbidden address).
    Permanent(String),
}

/// Publishes a session to one RTMP(S) destination.
pub struct RtmpEgress {
    id: String,
    target: Target,
    policy: ConnectPolicy,
    headers: Option<StreamHeaders>,
    connection: Option<Connection>,
    phase: EgressPhase,
    detail: Option<String>,
    reconnects: u64,
    backoff: Duration,
    next_attempt: Option<Instant>,
    /// Video is skipped until a keyframe after every (re)connect.
    waiting_for_key: bool,
    /// Timestamp of the first tag on the current connection; the stream restarts at 0.
    ts_origin: Option<u32>,
    permanent_failure: bool,
}

impl RtmpEgress {
    /// An egress for `target`, identified as `id` in status reports.
    pub fn new(id: impl Into<String>, target: Target, policy: ConnectPolicy) -> Self {
        RtmpEgress {
            id: id.into(),
            target,
            policy,
            headers: None,
            connection: None,
            phase: EgressPhase::Starting,
            detail: None,
            reconnects: 0,
            backoff: policy.reconnect_min,
            next_attempt: None,
            waiting_for_key: true,
            ts_origin: None,
            permanent_failure: false,
        }
    }

    /// The destination (key redacted in `Debug`/`Display`).
    pub fn target(&self) -> &Target {
        &self.target
    }

    /// Connects, publishes the preamble and goes live, or records the failure and
    /// schedules the next attempt. Errors only on a permanent failure.
    async fn try_connect(&mut self) -> Result<(), EgressError> {
        match self.connect_and_prime().await {
            Ok(connection) => {
                if self.phase == EgressPhase::Reconnecting {
                    self.reconnects += 1;
                }
                tracing::info!(id = %self.id, target = %self.target.tc_url(),
                    reconnects = self.reconnects, "rtmp egress publishing");
                self.connection = Some(connection);
                self.phase = EgressPhase::Live;
                self.detail = None;
                self.backoff = self.policy.reconnect_min;
                self.next_attempt = None;
                self.waiting_for_key = true;
                self.ts_origin = None;
                Ok(())
            }
            Err(Failure::Permanent(why)) => {
                tracing::warn!(id = %self.id, target = %self.target.tc_url(), %why,
                    "rtmp egress refused for good");
                self.permanent_failure = true;
                self.phase = EgressPhase::Failed;
                self.detail = Some(why.clone());
                Err(EgressError::Rejected(why))
            }
            Err(Failure::Transient(why)) => {
                self.schedule_retry(why);
                Ok(())
            }
        }
    }

    fn schedule_retry(&mut self, why: String) {
        tracing::warn!(id = %self.id, target = %self.target.tc_url(), %why,
            retry_in = ?self.backoff, "rtmp egress lost; will retry");
        self.connection = None;
        self.phase = EgressPhase::Reconnecting;
        self.detail = Some(why);
        self.next_attempt = Some(Instant::now() + self.backoff);
        self.backoff = (self.backoff * 2).min(self.policy.reconnect_max);
    }

    async fn connect_and_prime(&mut self) -> Result<Connection, Failure> {
        let mut connection = connect(&self.target, &self.policy).await?;
        let headers = self
            .headers
            .clone()
            .ok_or_else(|| Failure::Permanent("egress started without headers".into()))?;
        let metadata = stream_metadata(&headers.info);
        let mut outputs = Vec::new();
        guarded(|| {
            connection
                .publisher
                .publish_metadata(&metadata, &mut outputs)
        })?;
        apply(&mut connection, outputs, &self.policy).await?;
        let mut preamble = vec![&headers.video_sequence_header];
        if let Some(audio) = &headers.audio_sequence_header {
            preamble.push(audio);
        }
        for tag in preamble {
            let mut outputs = Vec::new();
            let tag = Tag {
                kind: tag.kind,
                timestamp_ms: 0,
                body: &tag.body,
            };
            guarded(|| connection.publisher.publish_tag(&tag, &mut outputs))?;
            apply(&mut connection, outputs, &self.policy).await?;
        }
        Ok(connection)
    }

    fn due(&self) -> bool {
        self.next_attempt.is_none_or(|at| Instant::now() >= at)
    }
}

/// TCP (+ TLS), then the handshake, `connect` and `publish`.
async fn connect(target: &Target, policy: &ConnectPolicy) -> Result<Connection, Failure> {
    let addrs = resolve(target, policy.allow_private).await?;
    let mut tcp = None;
    let mut last = String::from("no address");
    for addr in addrs {
        match timeout(policy.connect_timeout, TcpStream::connect(addr)).await {
            Ok(Ok(stream)) => {
                tcp = Some(stream);
                break;
            }
            Ok(Err(e)) => last = format!("connect failed: {}", e.kind()),
            Err(_) => last = "connect timed out".to_owned(),
        }
    }
    let tcp = tcp.ok_or(Failure::Transient(last))?;
    let _ = tcp.set_nodelay(true);
    let socket: Box<dyn Socket> = if target.tls {
        let name = ServerName::try_from(target.host.clone())
            .map_err(|_| Failure::Permanent("invalid TLS server name".into()))?;
        let connector = TlsConnector::from(tls_config());
        match timeout(policy.connect_timeout, connector.connect(name, tcp)).await {
            Ok(Ok(tls)) => Box::new(tls),
            Ok(Err(e)) => {
                return Err(Failure::Transient(format!("tls failed: {}", e.kind())));
            }
            Err(_) => return Err(Failure::Transient("tls timed out".into())),
        }
    } else {
        Box::new(tcp)
    };

    let mut outputs = Vec::new();
    let publisher = Publisher::new(target.clone(), &mut outputs)
        .map_err(|e| Failure::Transient(format!("rtmp: {}", e.kind())))?;
    let mut connection = Connection {
        socket,
        publisher,
        inbound: 0,
        read_buf: vec![0; READ_CHUNK],
    };
    apply(&mut connection, outputs, policy).await?;
    let publishing = async {
        while connection.publisher.state() != State::Publishing {
            read_some(&mut connection, policy).await?;
        }
        Ok::<(), Failure>(())
    };
    match timeout(policy.publish_timeout, publishing).await {
        Ok(Ok(())) => Ok(connection),
        Ok(Err(failure)) => Err(failure),
        Err(_) => Err(Failure::Transient("publish timed out".into())),
    }
}

#[async_trait]
impl Egress for RtmpEgress {
    fn id(&self) -> &str {
        &self.id
    }

    fn kind(&self) -> EgressKind {
        EgressKind::Rtmp
    }

    async fn start(&mut self, headers: &StreamHeaders) -> Result<(), EgressError> {
        self.headers = Some(headers.clone());
        self.try_connect().await
    }

    async fn push(&mut self, _seq: u32, tag: &TagBuf) -> Result<Delivery, EgressError> {
        if self.permanent_failure {
            return Err(EgressError::Closed);
        }
        if self.connection.is_none() {
            if !self.due() {
                return Ok(Delivery::Dropped);
            }
            self.try_connect().await?;
            if self.connection.is_none() {
                return Ok(Delivery::Dropped);
            }
        }
        let class = tag.as_tag().class()?;
        if self.waiting_for_key {
            match class {
                TagClass::Delta => return Ok(Delivery::Dropped),
                TagClass::Key => self.waiting_for_key = false,
                _ => {}
            }
        }
        let origin = *self.ts_origin.get_or_insert(tag.timestamp_ms);
        let rebased = Tag {
            kind: tag.kind,
            timestamp_ms: tag.timestamp_ms.saturating_sub(origin),
            body: &tag.body,
        };
        let connection = self.connection.as_mut().expect("connected");
        let result = async {
            poll_inbound(connection, &self.policy).await?;
            let mut outputs = Vec::new();
            guarded(|| connection.publisher.publish_tag(&rebased, &mut outputs))?;
            apply(connection, outputs, &self.policy).await
        }
        .await;
        match result {
            Ok(()) => Ok(Delivery::Sent),
            Err(Failure::Permanent(why)) => {
                self.permanent_failure = true;
                self.connection = None;
                self.phase = EgressPhase::Failed;
                self.detail = Some(why.clone());
                Err(EgressError::Rejected(why))
            }
            Err(Failure::Transient(why)) => {
                self.schedule_retry(why);
                Ok(Delivery::Dropped)
            }
        }
    }

    async fn stop(&mut self) -> Result<(), EgressError> {
        if let Some(mut connection) = self.connection.take() {
            let mut outputs = Vec::new();
            if guarded(|| connection.publisher.stop(&mut outputs)).is_ok() {
                let _ = apply(&mut connection, outputs, &self.policy).await;
            }
            let _ = timeout(Duration::from_secs(1), connection.socket.shutdown()).await;
        }
        if !self.permanent_failure {
            self.phase = EgressPhase::Stopped;
        }
        Ok(())
    }

    fn health(&self) -> Health {
        Health {
            phase: self.phase,
            reconnects: self.reconnects,
            detail: self.detail.clone(),
        }
    }
}

/// Runs a publisher call, turning a panic inside `rml_rtmp` (its decoders are not
/// hardened against hostile input) into a transient failure of this connection.
fn guarded(
    call: impl FnOnce() -> Result<(), prostor_protocol::rtmp::Error>,
) -> Result<(), Failure> {
    match catch_unwind(AssertUnwindSafe(call)) {
        Ok(Ok(())) => Ok(()),
        Ok(Err(e)) => Err(Failure::Transient(format!("rtmp: {}", e.kind()))),
        Err(_) => Err(Failure::Transient("rtmp library panicked".into())),
    }
}

/// Writes every `Send` output and inspects the events.
async fn apply(
    connection: &mut Connection,
    outputs: Vec<Output>,
    policy: &ConnectPolicy,
) -> Result<(), Failure> {
    for output in outputs {
        match output {
            Output::Send { bytes, .. } => {
                match timeout(policy.write_timeout, connection.socket.write_all(&bytes)).await {
                    Ok(Ok(())) => {}
                    Ok(Err(e)) => {
                        return Err(Failure::Transient(format!("write failed: {}", e.kind())));
                    }
                    Err(_) => return Err(Failure::Transient("write timed out".into())),
                }
            }
            Output::Event(Event::Rejected { code, description }) => {
                let why = format!("rejected: {code:?} ({description})");
                return Err(if is_permanent(code) {
                    Failure::Permanent(why)
                } else {
                    Failure::Transient(why)
                });
            }
            Output::Event(_) => {}
        }
    }
    Ok(())
}

fn is_permanent(code: RejectCode) -> bool {
    matches!(
        code,
        RejectCode::PublishBadName
            | RejectCode::PublishDenied
            | RejectCode::PublishRejected
            | RejectCode::ConnectRejected
    )
}

/// Reads once from the socket and feeds the bytes to the publisher.
async fn read_some(connection: &mut Connection, policy: &ConnectPolicy) -> Result<(), Failure> {
    let n = connection
        .socket
        .read(&mut connection.read_buf)
        .await
        .map_err(|e| Failure::Transient(format!("read failed: {}", e.kind())))?;
    feed(connection, n, policy).await
}

/// Consumes whatever the server has sent without waiting for more.
async fn poll_inbound(connection: &mut Connection, policy: &ConnectPolicy) -> Result<(), Failure> {
    loop {
        // A zero timeout polls the read exactly once: ready bytes are taken, otherwise
        // the read is cancelled without consuming anything.
        match timeout(
            Duration::ZERO,
            connection.socket.read(&mut connection.read_buf),
        )
        .await
        {
            Ok(Ok(n)) => feed(connection, n, policy).await?,
            Ok(Err(e)) => {
                return Err(Failure::Transient(format!("read failed: {}", e.kind())));
            }
            Err(_) => return Ok(()),
        }
    }
}

async fn feed(
    connection: &mut Connection,
    n: usize,
    policy: &ConnectPolicy,
) -> Result<(), Failure> {
    if n == 0 {
        return Err(Failure::Transient("server closed the connection".into()));
    }
    connection.inbound += n;
    if connection.inbound > INBOUND_LIMIT {
        return Err(Failure::Transient("server sent too much data".into()));
    }
    let bytes = connection.read_buf[..n].to_vec();
    let mut outputs = Vec::new();
    guarded(|| connection.publisher.handle_input(&bytes, &mut outputs))?;
    apply(connection, outputs, policy).await
}

/// Resolves the target and vets every address against the policy. A name that resolves
/// to any forbidden address is refused as a whole (DNS rebinding).
async fn resolve(target: &Target, allow_private: bool) -> Result<Vec<SocketAddr>, Failure> {
    let addrs: Vec<SocketAddr> = match target.host.parse::<IpAddr>() {
        Ok(ip) => vec![SocketAddr::new(ip, target.port)],
        Err(_) => tokio::net::lookup_host((target.host.as_str(), target.port))
            .await
            .map_err(|e| Failure::Transient(format!("dns failed: {}", e.kind())))?
            .collect(),
    };
    if addrs.is_empty() {
        return Err(Failure::Transient("dns returned no address".into()));
    }
    if !allow_private && addrs.iter().any(|a| is_private(a.ip())) {
        return Err(Failure::Permanent(
            "destination resolves to a loopback, link-local or private address".into(),
        ));
    }
    Ok(addrs)
}

/// Addresses an egress must not connect to unless explicitly allowed.
pub fn is_private(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => is_private_v4(v4),
        IpAddr::V6(v6) => match v6.to_ipv4_mapped() {
            Some(v4) => is_private_v4(v4),
            None => {
                v6.is_loopback()
                    || v6.is_unspecified()
                    || v6.is_multicast()
                    || v6.is_unique_local()
                    || v6.is_unicast_link_local()
            }
        },
    }
}

fn is_private_v4(ip: Ipv4Addr) -> bool {
    let [a, b, _, _] = ip.octets();
    ip.is_private()
        || ip.is_loopback()
        || ip.is_link_local()
        || ip.is_unspecified()
        || ip.is_broadcast()
        || ip.is_multicast()
        || ip.is_documentation()
        || a == 0
        || (a == 100 && (64..=127).contains(&b))
        || (a == 198 && (18..=19).contains(&b))
}

fn tls_config() -> Arc<ClientConfig> {
    static CONFIG: OnceLock<Arc<ClientConfig>> = OnceLock::new();
    CONFIG
        .get_or_init(|| {
            let mut roots = RootCertStore::empty();
            roots.extend(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());
            let provider = Arc::new(tokio_rustls::rustls::crypto::ring::default_provider());
            let config = ClientConfig::builder_with_provider(provider)
                .with_safe_default_protocol_versions()
                .expect("ring supports the default protocol versions")
                .with_root_certificates(roots)
                .with_no_client_auth();
            Arc::new(config)
        })
        .clone()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn private_address_detection() {
        let private = [
            "127.0.0.1",
            "10.1.2.3",
            "172.16.0.1",
            "192.168.1.1",
            "169.254.1.1",
            "0.0.0.0",
            "100.64.0.1",
            "198.18.0.1",
            "224.0.0.1",
            "::1",
            "::",
            "fc00::1",
            "fd12::1",
            "fe80::1",
            "::ffff:10.0.0.1",
            "ff02::1",
        ];
        for ip in private {
            assert!(is_private(ip.parse().unwrap()), "{ip}");
        }
        for ip in ["8.8.8.8", "52.223.0.1", "2606:4700::1111", "::ffff:8.8.8.8"] {
            assert!(!is_private(ip.parse().unwrap()), "{ip}");
        }
    }

    #[test]
    fn rejections_are_classified() {
        assert!(is_permanent(RejectCode::PublishBadName));
        assert!(is_permanent(RejectCode::ConnectRejected));
        assert!(!is_permanent(RejectCode::ConnectFailed));
        assert!(!is_permanent(RejectCode::Other));
    }

    #[test]
    fn tls_config_builds_once() {
        let a = tls_config();
        let b = tls_config();
        assert!(Arc::ptr_eq(&a, &b));
    }
}
