use std::{
    net::SocketAddr,
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
    time::{Duration, Instant},
};

use anyhow::{Context, Result, ensure};
use bytes::Buf;
use prost::Message;
use stem_stake::{Hash32, jolteon::Point};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{TcpListener, TcpStream},
    sync::{Semaphore, mpsc, oneshot},
    task::JoinHandle,
    time::timeout,
};

use super::{
    peer_discovery::{
        AUTHENTICATED_PEER, Authentication, CHALLENGE_BYTES, MAX_SIGNED_ENDPOINT_BYTES,
        SIGNED_CHALLENGE_BYTES,
    },
    trace::{Reception, Tag, Trace},
    wire::{self, Frame},
};

const QUEUE: usize = 8;
const INBOUND_QUEUE: usize = 128;
const CONTROL_BURST: usize = 8;
const IO_TIMEOUT: Duration = Duration::from_secs(6);
const MAX_ENDPOINT_RESPONSE: usize = SIGNED_CHALLENGE_BYTES + 32;

pub struct Incoming {
    pub frame: Frame,
    pub reply: Option<oneshot::Sender<Vec<u8>>>,
    pub submit_received_at: Option<Instant>,
    pub control_received_at: Option<Instant>,
    pub trace_read: Option<(Instant, usize)>,
    pub peer_key: Option<Hash32>,
    pub signed_endpoint: Option<Arc<Vec<u8>>>,
}

impl Incoming {
    pub(super) fn reception(&self) -> Option<Reception> {
        let (read, bytes) = self.trace_read?;
        Some(Reception {
            read,
            bytes,
            queued: self.control_received_at?,
            handling: Instant::now(),
        })
    }
}

pub(super) struct Inbox {
    control: mpsc::Receiver<Incoming>,
    application: mpsc::Receiver<Incoming>,
    burst: usize,
    #[cfg(feature = "jolteon-commit-pipeline")]
    deferred: std::collections::VecDeque<Incoming>,
    #[cfg(feature = "jolteon-commit-pipeline")]
    deferred_bytes: usize,
}

impl Inbox {
    pub(super) async fn recv(&mut self) -> Option<Incoming> {
        #[cfg(feature = "jolteon-commit-pipeline")]
        if let Some(event) = self.deferred.pop_front() {
            self.deferred_bytes -= event.frame.body.len();
            return Some(event);
        }
        self.recv_live().await
    }

    pub(super) async fn recv_live(&mut self) -> Option<Incoming> {
        if self.burst == CONTROL_BURST
            && let Ok(event) = self.application.try_recv()
        {
            self.burst = 0;
            return Some(event);
        }
        tokio::select! {
            biased;
            Some(event) = self.control.recv() => {
                self.burst = (self.burst + 1).min(CONTROL_BURST);
                Some(event)
            }
            Some(event) = self.application.recv() => {
                self.burst = 0;
                Some(event)
            }
            else => None,
        }
    }

    #[cfg(feature = "jolteon-commit-pipeline")]
    pub(super) fn can_defer(&self) -> bool {
        self.deferred.len() < CONTROL_BURST && self.deferred_bytes < wire::MAX_FRAME
    }

    #[cfg(feature = "jolteon-commit-pipeline")]
    pub(super) fn defer(&mut self, event: Incoming) {
        debug_assert!(self.can_defer());
        self.deferred_bytes += event.frame.body.len();
        self.deferred.push_back(event);
    }
}

struct Outbound {
    frame: Arc<Vec<u8>>,
    kind: u32,
    tag: Option<Tag>,
}

struct PeerQueues {
    control: mpsc::Sender<Outbound>,
    relay: mpsc::Sender<Outbound>,
    tasks: [JoinHandle<()>; 2],
}

impl PeerQueues {
    fn connect(
        address: SocketAddr,
        traffic: &Arc<Traffic>,
        endpoint_identity: Option<(Hash32, Arc<Authentication>)>,
    ) -> Self {
        let (control, control_in) = mpsc::channel(QUEUE);
        let (relay, relay_in) = mpsc::channel(QUEUE);
        // Independent streams also keep TCP backpressure off the control path.
        let tasks = [
            tokio::spawn(send(
                address,
                control_in,
                traffic.clone(),
                endpoint_identity.clone(),
            )),
            tokio::spawn(send(address, relay_in, traffic.clone(), endpoint_identity)),
        ];
        Self {
            control,
            relay,
            tasks,
        }
    }
}

impl Drop for PeerQueues {
    fn drop(&mut self) {
        for task in &self.tasks {
            task.abort();
        }
    }
}

#[derive(Clone)]
struct IncomingQueues {
    control: mpsc::Sender<Incoming>,
    application: mpsc::Sender<Incoming>,
}

#[derive(Default)]
pub struct Traffic {
    pub sent: AtomicU64,
    pub received: AtomicU64,
    pub dropped: AtomicU64,
    pub(super) trace: Option<Trace>,
    kinds: [KindTraffic; wire::QUERY as usize + 1],
}

#[derive(Default)]
struct KindTraffic {
    sent_frames: AtomicU64,
    sent_bytes: AtomicU64,
    received_frames: AtomicU64,
    received_bytes: AtomicU64,
    enqueue_drops: AtomicU64,
    write_drops: AtomicU64,
}

impl Traffic {
    fn kind(&self, kind: u32) -> Option<&KindTraffic> {
        self.trace.as_ref()?;
        self.kinds.get(kind as usize)
    }

    pub(super) fn by_kind(&self) -> Option<Vec<serde_json::Value>> {
        self.trace.as_ref()?;
        Some(
            self.kinds
                .iter()
                .enumerate()
                .map(|(kind, row)| {
                    serde_json::json!({
                        "kind": kind,
                        "sent_frames": row.sent_frames.load(Ordering::Relaxed),
                        "sent_bytes": row.sent_bytes.load(Ordering::Relaxed),
                        "received_frames": row.received_frames.load(Ordering::Relaxed),
                        "received_bytes": row.received_bytes.load(Ordering::Relaxed),
                        "enqueue_drops": row.enqueue_drops.load(Ordering::Relaxed),
                        "write_drops": row.write_drops.load(Ordering::Relaxed),
                    })
                })
                .collect(),
        )
    }
}

pub struct Network {
    pub traffic: Arc<Traffic>,
    own: u16,
    pub epoch: u64,
    peers: Vec<Option<PeerQueues>>,
    authentication: Option<Arc<Authentication>>,
}

impl Network {
    pub(super) async fn listen(
        own: u16,
        addresses: &[SocketAddr],
        epoch: u64,
        authentication: Option<Authentication>,
        trace: Option<Trace>,
    ) -> Result<(Self, Inbox)> {
        let (control_sender, control) = mpsc::channel(INBOUND_QUEUE);
        let (application_sender, application) = mpsc::channel(INBOUND_QUEUE);
        let sender = IncomingQueues {
            control: control_sender,
            application: application_sender,
        };
        let listener = TcpListener::bind(addresses[usize::from(own)]).await?;
        let traffic = Arc::new(Traffic {
            trace,
            ..Traffic::default()
        });
        let counters = traffic.clone();
        let authentication = authentication.map(Arc::new);
        let inbound_authentication = authentication.clone();
        let additional_peers = authentication
            .as_ref()
            .map_or(0, |_| 3 * stem_stake::jolteon::MAX_MEMBERS);
        let limit = Arc::new(Semaphore::new(
            4 * (addresses.len() + additional_peers) + 64,
        ));
        tokio::spawn(async move {
            while let Ok((socket, _)) = listener.accept().await {
                let Ok(permit) = limit.clone().try_acquire_owned() else {
                    continue;
                };
                let sender = sender.clone();
                let counters = counters.clone();
                let authentication = inbound_authentication.clone();
                tokio::spawn(async move {
                    let _permit = permit;
                    let _ = receive(socket, sender, counters, authentication.as_deref()).await;
                });
            }
        });
        let peers = addresses
            .iter()
            .enumerate()
            .map(|(index, &addr)| {
                (index != usize::from(own)).then(|| PeerQueues::connect(addr, &traffic, None))
            })
            .collect();
        Ok((
            Self {
                traffic,
                own,
                epoch,
                peers,
                authentication,
            },
            Inbox {
                control,
                application,
                burst: 0,
                #[cfg(feature = "jolteon-commit-pipeline")]
                deferred: Default::default(),
                #[cfg(feature = "jolteon-commit-pipeline")]
                deferred_bytes: 0,
            },
        ))
    }

    pub(super) fn update_endpoint(&mut self, peer: u16, endpoint: Option<(SocketAddr, Hash32)>) {
        debug_assert_ne!(peer, self.own);
        let index = usize::from(peer);
        if self.peers.len() <= index {
            self.peers.resize_with(index + 1, || None);
        }
        self.peers[index] = endpoint.map(|(address, key)| {
            let identity = self.authentication.as_ref().map(|auth| (key, auth.clone()));
            PeerQueues::connect(address, &self.traffic, identity)
        });
    }

    pub(super) fn has_route(&self, peer: u16) -> bool {
        self.peers.get(usize::from(peer)).is_some_and(Option::is_some)
    }

    pub(super) fn frame(
        &self,
        epoch: u64,
        kind: u32,
        body: impl Into<bytes::Bytes>,
    ) -> Arc<Vec<u8>> {
        let body = body.into();
        let (sender, body) = match &self.authentication {
            Some(auth) => (AUTHENTICATED_PEER, auth.sign(epoch, kind, body)),
            None => (u32::from(self.own), body),
        };
        Arc::new(
            Frame {
                sender,
                kind,
                body,
                epoch,
            }
            .encode_to_vec(),
        )
    }

    pub fn broadcast(&self, kind: u32, body: Vec<u8>) {
        self.broadcast_at(self.epoch, kind, body);
    }

    pub fn broadcast_at(&self, epoch: u64, kind: u32, body: Vec<u8>) {
        self.broadcast_frame(kind, self.frame(epoch, kind, body));
    }

    pub(super) fn broadcast_frame(&self, kind: u32, msg: Arc<Vec<u8>>) {
        self.broadcast_tagged(kind, msg, None);
    }

    pub(super) fn broadcast_point(&self, kind: u32, point: Point, body: Vec<u8>) {
        self.broadcast_tagged(kind, self.frame(self.epoch, kind, body), Some(point));
    }

    fn broadcast_tagged(&self, kind: u32, msg: Arc<Vec<u8>>, point: Option<Point>) {
        for (id, peer) in self.peers.iter().enumerate() {
            if let Some(peer) = peer {
                let tag = point.map(|point| Tag {
                    epoch: self.epoch,
                    point,
                    kind,
                    peer: id as u16,
                });
                self.enqueue(peer, kind, msg.clone(), tag);
            }
        }
    }

    pub fn send(&self, peer: u16, kind: u32, body: Vec<u8>) {
        self.send_at(self.epoch, peer, kind, body);
    }

    pub fn send_at(&self, epoch: u64, peer: u16, kind: u32, body: Vec<u8>) {
        if let Some(Some(peer)) = self.peers.get(usize::from(peer)) {
            self.enqueue(peer, kind, self.frame(epoch, kind, body), None);
        }
    }

    pub(super) fn send_point_at(
        &self,
        epoch: u64,
        peer: u16,
        kind: u32,
        point: Point,
        body: Vec<u8>,
    ) {
        if let Some(Some(queue)) = self.peers.get(usize::from(peer)) {
            self.enqueue(
                queue,
                kind,
                self.frame(epoch, kind, body),
                Some(Tag {
                    epoch,
                    point,
                    kind,
                    peer,
                }),
            );
        }
    }

    pub(super) fn relay(&self, routes: &[Option<u16>], body: bytes::Bytes) {
        let mut frame = None;
        for (index, route) in routes.iter().enumerate() {
            if let Some(id) = route
                && !routes[..index].contains(route)
                && let Some(Some(peer)) = self.peers.get(usize::from(*id))
            {
                let message = frame
                    .get_or_insert_with(|| self.frame(self.epoch, wire::RELAY_TX, body.clone()));
                self.enqueue(peer, wire::RELAY_TX, message.clone(), None);
            }
        }
    }

    fn enqueue(&self, peer: &PeerQueues, kind: u32, frame: Arc<Vec<u8>>, tag: Option<Tag>) {
        let queue = if kind == wire::RELAY_TX {
            &peer.relay
        } else {
            &peer.control
        };
        let queued = self
            .traffic
            .trace
            .as_ref()
            .zip(tag)
            .map(|(trace, tag)| (trace, tag, Instant::now(), frame.len() + 4));
        if queue.try_send(Outbound { frame, kind, tag }).is_err() {
            self.traffic.dropped.fetch_add(1, Ordering::Relaxed);
            if let Some(row) = self.traffic.kind(kind) {
                row.enqueue_drops.fetch_add(1, Ordering::Relaxed);
            }
            if let Some((trace, tag, time, bytes)) = queued {
                trace.wire(tag, "tx_queue_drop", time, bytes);
            }
        } else if let Some((trace, tag, time, bytes)) = queued {
            trace.wire(tag, "tx_queued", time, bytes);
        }
    }
}

async fn receive(
    mut stream: TcpStream,
    incoming: IncomingQueues,
    traffic: Arc<Traffic>,
    auth: Option<&Authentication>,
) -> Result<()> {
    let mut submissions = 0;
    let mut first = true;
    loop {
        let frame_timeout = if submissions == 0 {
            IO_TIMEOUT
        } else {
            wire::SUBMIT_IDLE
        };
        let raw = timeout(frame_timeout, read(&mut stream)).await??;
        let bytes = raw.len() + 4;
        let trace_read = traffic.trace.as_ref().map(|_| (Instant::now(), bytes));
        traffic
            .received
            .fetch_add(4 + raw.len() as u64, Ordering::Relaxed);
        let mut frame: Frame = wire::canonical_bytes(raw.into())?;
        if let Some(row) = traffic.kind(frame.kind) {
            row.received_frames.fetch_add(1, Ordering::Relaxed);
            row.received_bytes
                .fetch_add(bytes as u64, Ordering::Relaxed);
        }
        let first_frame = std::mem::replace(&mut first, false);
        if submissions != 0 {
            ensure!(
                frame.sender == wire::CLIENT && frame.kind == wire::SUBMIT,
                "submission connection expects SUBMIT"
            );
        }
        if frame.sender == wire::CLIENT {
            ensure!(
                matches!(
                    frame.kind,
                    wire::SUBMIT
                        | wire::STATUS
                        | wire::FINALITY
                        | wire::HEADER
                        | wire::HANDOFF_PROOF
                        | wire::QUERY
                ),
                "unsupported client request"
            );
            let submit_received_at = (frame.kind == wire::SUBMIT).then(Instant::now);
            if submit_received_at.is_some() {
                if submissions == 0 {
                    stream.set_nodelay(true)?;
                }
                submissions += 1;
            }
            let (sender, receiver) = oneshot::channel();
            incoming
                .application
                .send(Incoming {
                    frame,
                    reply: Some(sender),
                    submit_received_at,
                    control_received_at: None,
                    trace_read: None,
                    peer_key: None,
                    signed_endpoint: None,
                })
                .await?;
            let response = timeout(IO_TIMEOUT, receiver).await??;
            timeout(IO_TIMEOUT, write(&mut stream, &response)).await??;
            if submit_received_at.is_none() || submissions == wire::SUBMIT_SESSION_REQUESTS {
                return Ok(());
            }
            continue;
        }
        if frame.kind == wire::ENDPOINT_CHALLENGE {
            let auth = auth.context("endpoint authentication is disabled")?;
            ensure!(first_frame && frame.epoch == 0 && frame.body.len() == SIGNED_CHALLENGE_BYTES,
                "invalid endpoint challenge");
            let (_, challenge) = auth.verify(&frame)?;
            let response = Frame {
                sender: AUTHENTICATED_PEER,
                kind: wire::ENDPOINT_RESPONSE,
                epoch: 0,
                body: auth.sign(0, wire::ENDPOINT_RESPONSE, challenge),
            };
            timeout(IO_TIMEOUT, write(&mut stream, &response.encode_to_vec())).await??;
            continue;
        }
        ensure!(
            matches!(frame.kind, wire::PROPOSAL..=wire::PEER_ENDPOINT),
            "unsupported peer frame"
        );
        let endpoint = frame.kind == wire::PEER_ENDPOINT;
        ensure!(!endpoint || (auth.is_some() && frame.body.len() <= MAX_SIGNED_ENDPOINT_BYTES), "invalid endpoint envelope");
        let (peer_key, signed_endpoint) = if let Some(auth) = auth {
            let (author, body) = auth.verify(&frame)?;
            let signed_endpoint = endpoint.then(|| Arc::new(frame.encode_to_vec()));
            frame.body = body;
            (Some(author), signed_endpoint)
        } else {
            (None, None)
        };
        let queue = if frame.kind == wire::RELAY_TX {
            &incoming.application
        } else {
            &incoming.control
        };
        let control_received_at = (frame.kind != wire::RELAY_TX).then(Instant::now);
        queue
            .send(Incoming {
                frame,
                reply: None,
                submit_received_at: None,
                control_received_at,
                trace_read,
                peer_key,
                signed_endpoint,
            })
            .await?;
    }
}

async fn send(
    address: SocketAddr,
    mut frames: mpsc::Receiver<Outbound>,
    traffic: Arc<Traffic>,
    endpoint_identity: Option<(Hash32, Arc<Authentication>)>,
) {
    let mut socket = None;
    while let Some(Outbound { frame, kind, tag }) = frames.recv().await {
        let bytes = frame.len() + 4;
        let trace = traffic.trace.as_ref().zip(tag);
        if let Some((trace, tag)) = trace {
            trace.wire(tag, "tx_dequeued", Instant::now(), bytes);
        }
        if socket.is_none() {
            socket = timeout(Duration::from_millis(200), TcpStream::connect(address))
                .await
                .ok()
                .and_then(Result::ok);
            if let Some(stream) = &mut socket {
                let _ = stream.set_nodelay(true);
                if let Some((expected, auth)) = &endpoint_identity
                    && !matches!(timeout(IO_TIMEOUT, check_endpoint(stream, expected, auth)).await, Ok(Ok(())))
                {
                    socket = None;
                }
            }
        }
        let written = if let Some(stream) = &mut socket {
            if let Some((trace, tag)) = trace {
                trace.wire(tag, "tx_write_begin", Instant::now(), bytes);
            }
            matches!(timeout(IO_TIMEOUT, write(stream, &frame)).await, Ok(Ok(())))
        } else {
            false
        };
        if let Some((trace, tag)) = trace {
            trace.wire(
                tag,
                if written {
                    "tx_kernel_write_done"
                } else {
                    "tx_write_drop"
                },
                Instant::now(),
                bytes,
            );
        }
        if written {
            traffic
                .sent
                .fetch_add(4 + frame.len() as u64, Ordering::Relaxed);
            if let Some(row) = traffic.kind(kind) {
                row.sent_frames.fetch_add(1, Ordering::Relaxed);
                row.sent_bytes.fetch_add(bytes as u64, Ordering::Relaxed);
            }
        } else {
            socket = None;
            traffic.dropped.fetch_add(1, Ordering::Relaxed);
            if let Some(row) = traffic.kind(kind) {
                row.write_drops.fetch_add(1, Ordering::Relaxed);
            }
        }
    }
}

async fn check_endpoint(stream: &mut TcpStream, expected: &Hash32, auth: &Authentication) -> Result<()> {
    let challenge = rand::random::<[u8; CHALLENGE_BYTES]>();
    let request = Frame {
        sender: AUTHENTICATED_PEER,
        kind: wire::ENDPOINT_CHALLENGE,
        epoch: 0,
        body: auth.sign(0, wire::ENDPOINT_CHALLENGE, bytes::Bytes::copy_from_slice(&challenge)),
    };
    write(stream, &request.encode_to_vec()).await?;
    let frame: Frame = wire::canonical_bytes(read_limited(stream, MAX_ENDPOINT_RESPONSE).await?.into())?;
    ensure!(frame.kind == wire::ENDPOINT_RESPONSE && frame.epoch == 0
        && frame.body.len() == SIGNED_CHALLENGE_BYTES, "invalid endpoint challenge response");
    let (author, reply) = auth.verify(&frame)?;
    ensure!(&author == expected && reply.as_ref() == challenge, "endpoint identity mismatch");
    Ok(())
}

async fn read(stream: &mut TcpStream) -> Result<Vec<u8>> {
    read_limited(stream, wire::MAX_FRAME).await
}

async fn read_limited(stream: &mut TcpStream, limit: usize) -> Result<Vec<u8>> {
    let length = stream.read_u32().await? as usize;
    ensure!(
        (1..=limit).contains(&length),
        "invalid consensus frame length"
    );
    let mut bytes = vec![0; length];
    stream.read_exact(&mut bytes).await?;
    Ok(bytes)
}

async fn write(stream: &mut TcpStream, bytes: &[u8]) -> Result<()> {
    ensure!(bytes.len() <= wire::MAX_FRAME, "oversize consensus frame");
    let prefix = (bytes.len() as u32).to_be_bytes();
    stream
        .write_all_buf(&mut Buf::chain(prefix.as_slice(), bytes))
        .await
        .context("write consensus frame")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn a_learned_endpoint_must_prove_the_announcing_key() -> Result<()> {
        let listener = TcpListener::bind("127.0.0.1:0").await?;
        let address = listener.local_addr()?;
        let authentication = |seed| Authentication {
            network: [1; 32],
            genesis: [2; 32],
            key: ed25519_dalek::SigningKey::from_bytes(&[seed; 32]),
        };
        let server = authentication(3);
        let expected = server.key.verifying_key().to_bytes();
        let requests = async {
            let (mut oversized, _) = listener.accept().await?;
            let _ = read(&mut oversized).await?;
            oversized.write_u32((MAX_ENDPOINT_RESPONSE + 1) as u32).await?;
            let (control, _control_rx) = mpsc::channel(1);
            let (application, _app_rx) = mpsc::channel(1);
            for _ in 0..2 {
                let (stream, _) = listener.accept().await?;
                let queues = IncomingQueues { control: control.clone(), application: application.clone() };
                let _ = receive(stream, queues, Arc::new(Traffic::default()), Some(&server)).await;
            }
            Ok::<_, anyhow::Error>(())
        };
        let client = async {
            let client = authentication(4);
            let mut oversized = TcpStream::connect(address).await?;
            let failure = check_endpoint(&mut oversized, &expected, &client).await.unwrap_err();
            assert!(failure.to_string().contains("invalid consensus frame length"));
            let mut wrong = TcpStream::connect(address).await?;
            assert!(check_endpoint(&mut wrong, &[5; 32], &client).await.is_err());
            drop(wrong);
            let mut right = TcpStream::connect(address).await?;
            check_endpoint(&mut right, &expected, &client).await
        };
        timeout(IO_TIMEOUT, async { tokio::try_join!(requests, client) }).await??;
        Ok(())
    }

    #[tokio::test]
    async fn control_overtakes_application_with_a_bounded_burst() -> Result<()> {
        let (control_tx, control) = mpsc::channel(CONTROL_BURST + 1);
        let (app_tx, application) = mpsc::channel(1);
        let mut inbox = Inbox {
            control,
            application,
            burst: 0,
            #[cfg(feature = "jolteon-commit-pipeline")]
            deferred: Default::default(),
            #[cfg(feature = "jolteon-commit-pipeline")]
            deferred_bytes: 0,
        };
        let event = |kind| Incoming {
            frame: Frame {
                kind,
                ..Frame::default()
            },
            reply: None,
            submit_received_at: None,
            control_received_at: None,
            trace_read: None,
            peer_key: None,
            signed_endpoint: None,
        };
        app_tx.try_send(event(wire::RELAY_TX))?;
        for _ in 0..=CONTROL_BURST {
            control_tx.try_send(event(wire::VOTE))?;
        }
        for _ in 0..CONTROL_BURST {
            assert_eq!(
                inbox.recv().await.context("control event")?.frame.kind,
                wire::VOTE
            );
        }
        assert_eq!(
            inbox.recv().await.context("application event")?.frame.kind,
            wire::RELAY_TX
        );
        drop(app_tx);
        drop(control_tx);
        assert_eq!(
            inbox.recv().await.context("last control event")?.frame.kind,
            wire::VOTE
        );
        assert!(inbox.recv().await.is_none());
        Ok(())
    }

    #[tokio::test]
    async fn submission_sessions_are_sequential_and_bounded() -> Result<()> {
        let listener = TcpListener::bind("127.0.0.1:0").await?;
        let (control, _control_queue) = mpsc::channel(1);
        let (application, mut requests) = mpsc::channel(1);
        let mut client = TcpStream::connect(listener.local_addr()?).await?;
        client.set_nodelay(true)?;
        let (server, _) = listener.accept().await?;
        let receive = tokio::spawn(receive(
            server,
            IncomingQueues {
                control,
                application,
            },
            Arc::new(Traffic::default()),
            None,
        ));
        let replies = tokio::spawn(async move {
            for index in 0..wire::SUBMIT_SESSION_REQUESTS {
                let request = requests.recv().await.context("missing submission")?;
                assert_eq!(request.frame.body.as_ref(), index.to_le_bytes());
                request
                    .reply
                    .context("missing submission reply")?
                    .send(index.to_be_bytes().to_vec())
                    .expect("client is waiting for a response");
            }
            Ok::<_, anyhow::Error>(())
        });
        timeout(IO_TIMEOUT, async {
            for index in 0..wire::SUBMIT_SESSION_REQUESTS {
                let frame = Frame {
                    sender: wire::CLIENT,
                    kind: wire::SUBMIT,
                    body: bytes::Bytes::copy_from_slice(&index.to_le_bytes()),
                    epoch: 0,
                };
                write(&mut client, &frame.encode_to_vec()).await?;
                assert_eq!(read(&mut client).await?, index.to_be_bytes());
            }
            assert_eq!(client.read(&mut [0]).await?, 0);
            Ok::<_, anyhow::Error>(())
        })
        .await??;
        receive.await??;
        replies.await??;
        Ok(())
    }

    #[tokio::test]
    async fn relay_backpressure_leaves_a_separate_control_socket() -> Result<()> {
        let peer = TcpListener::bind("127.0.0.1:0").await?;
        let local = "127.0.0.1:0".parse()?;
        let (network, _inbox) =
            Network::listen(0, &[local, peer.local_addr()?], 3, None, None).await?;
        let payload = vec![1; stem_ledger::MAX_TRANSACTION_BYTES];
        network.send(1, wire::RELAY_TX, payload.clone());
        let (_unread_relay, _) = timeout(Duration::from_secs(2), peer.accept()).await??;
        for _ in 0..=QUEUE {
            network.send(1, wire::RELAY_TX, payload.clone());
        }
        assert!(network.traffic.dropped.load(Ordering::Relaxed) > 0);
        network.send(1, wire::QUORUM, vec![2, 3]);
        let (mut control, _) = timeout(Duration::from_secs(2), peer.accept()).await??;
        let raw = timeout(Duration::from_secs(2), read(&mut control)).await??;
        let frame: Frame = wire::canonical(&raw)?;
        assert_eq!(
            (frame.sender, frame.epoch, frame.kind, frame.body),
            (0, 3, wire::QUORUM, bytes::Bytes::from_static(&[2, 3]))
        );
        Ok(())
    }

    #[tokio::test]
    async fn large_frame_preserves_following_frame() -> Result<()> {
        let listener = TcpListener::bind("127.0.0.1:0").await?;
        let mut client = TcpStream::connect(listener.local_addr()?).await?;
        let (mut server, _) = listener.accept().await?;
        let payload = vec![0xa5; 4 * 1024 * 1024];
        let send = async {
            write(&mut server, &payload).await?;
            write(&mut server, b"next").await
        };
        let receive = async {
            assert_eq!(read(&mut client).await?, payload);
            assert_eq!(read(&mut client).await?, b"next");
            Ok::<_, anyhow::Error>(())
        };
        timeout(IO_TIMEOUT, async { tokio::try_join!(send, receive) }).await??;
        Ok(())
    }
}
