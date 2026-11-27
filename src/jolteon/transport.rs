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
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{TcpListener, TcpStream},
    sync::{Semaphore, mpsc, oneshot},
    time::timeout,
};

use super::wire::{self, Frame};

const QUEUE: usize = 8;
const INBOUND_QUEUE: usize = 128;
const CONTROL_BURST: usize = 8;
const IO_TIMEOUT: Duration = Duration::from_secs(6);

pub struct Incoming {
    pub frame: Frame,
    pub reply: Option<oneshot::Sender<Vec<u8>>>,
    pub submit_received_at: Option<Instant>,
    pub control_received_at: Option<Instant>,
}

pub(super) struct Inbox {
    control: mpsc::Receiver<Incoming>,
    application: mpsc::Receiver<Incoming>,
    burst: usize,
}

impl Inbox {
    pub(super) async fn recv(&mut self) -> Option<Incoming> {
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
}

struct PeerQueues {
    control: mpsc::Sender<Arc<Vec<u8>>>,
    relay: mpsc::Sender<Arc<Vec<u8>>>,
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
}

pub struct Network {
    pub traffic: Arc<Traffic>,
    own: u16,
    pub epoch: u64,
    peers: Vec<Option<PeerQueues>>,
}

impl Network {
    pub(super) async fn listen(
        own: u16,
        addresses: &[SocketAddr],
        epoch: u64,
    ) -> Result<(Self, Inbox)> {
        let (control_sender, control) = mpsc::channel(INBOUND_QUEUE);
        let (application_sender, application) = mpsc::channel(INBOUND_QUEUE);
        let sender = IncomingQueues {
            control: control_sender,
            application: application_sender,
        };
        let listener = TcpListener::bind(addresses[usize::from(own)]).await?;
        let traffic = Arc::new(Traffic::default());
        let counters = traffic.clone();
        let limit = Arc::new(Semaphore::new(4 * addresses.len() + 64));
        tokio::spawn(async move {
            while let Ok((socket, _)) = listener.accept().await {
                let Ok(permit) = limit.clone().try_acquire_owned() else {
                    continue;
                };
                let sender = sender.clone();
                let counters = counters.clone();
                tokio::spawn(async move {
                    let _permit = permit;
                    let _ = receive(socket, sender, counters).await;
                });
            }
        });
        let peers = addresses
            .iter()
            .enumerate()
            .map(|(index, &addr)| {
                if index == usize::from(own) {
                    return None;
                }
                let (control, control_in) = mpsc::channel(QUEUE);
                let (relay, relay_in) = mpsc::channel(QUEUE);
                // Independent streams also keep TCP backpressure off the control path.
                tokio::spawn(send(addr, control_in, traffic.clone()));
                tokio::spawn(send(addr, relay_in, traffic.clone()));
                Some(PeerQueues { control, relay })
            })
            .collect();
        Ok((
            Self {
                traffic,
                own,
                epoch,
                peers,
            },
            Inbox {
                control,
                application,
                burst: 0,
            },
        ))
    }

    pub(super) fn frame(
        &self,
        epoch: u64,
        kind: u32,
        body: impl Into<bytes::Bytes>,
    ) -> Arc<Vec<u8>> {
        Arc::new(
            Frame {
                sender: u32::from(self.own),
                kind,
                body: body.into(),
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
        for peer in self.peers.iter().flatten() {
            self.enqueue(peer, kind, msg.clone());
        }
    }

    pub fn send(&self, peer: u16, kind: u32, body: Vec<u8>) {
        self.send_at(self.epoch, peer, kind, body);
    }

    pub fn send_at(&self, epoch: u64, peer: u16, kind: u32, body: Vec<u8>) {
        if let Some(Some(peer)) = self.peers.get(usize::from(peer)) {
            self.enqueue(peer, kind, self.frame(epoch, kind, body));
        }
    }

    pub(super) fn relay(&self, routes: &[Option<u16>], body: bytes::Bytes) {
        let mut frame = None;
        for (index, route) in routes.iter().enumerate() {
            if let Some(id) = route
                && !routes[..index].contains(route)
                && let Some(peer) = &self.peers[usize::from(*id)]
            {
                let message = frame
                    .get_or_insert_with(|| self.frame(self.epoch, wire::RELAY_TX, body.clone()));
                self.enqueue(peer, wire::RELAY_TX, message.clone());
            }
        }
    }

    fn enqueue(&self, peer: &PeerQueues, kind: u32, frame: Arc<Vec<u8>>) {
        let queue = if kind == wire::RELAY_TX {
            &peer.relay
        } else {
            &peer.control
        };
        if queue.try_send(frame).is_err() {
            self.traffic.dropped.fetch_add(1, Ordering::Relaxed);
        }
    }
}

async fn receive(
    mut stream: TcpStream,
    incoming: IncomingQueues,
    traffic: Arc<Traffic>,
) -> Result<()> {
    let mut submissions = 0;
    loop {
        let frame_timeout = if submissions == 0 {
            IO_TIMEOUT
        } else {
            wire::SUBMIT_IDLE
        };
        let raw = timeout(frame_timeout, read(&mut stream)).await??;
        traffic
            .received
            .fetch_add(4 + raw.len() as u64, Ordering::Relaxed);
        let frame: Frame = wire::canonical_bytes(raw.into())?;
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
                })
                .await?;
            let response = timeout(IO_TIMEOUT, receiver).await??;
            timeout(IO_TIMEOUT, write(&mut stream, &response)).await??;
            if submit_received_at.is_none() || submissions == wire::SUBMIT_SESSION_REQUESTS {
                return Ok(());
            }
            continue;
        }
        ensure!(
            matches!(frame.kind, wire::PROPOSAL..=wire::REPLAY),
            "unsupported peer frame"
        );
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
            })
            .await?;
    }
}

async fn send(
    address: SocketAddr,
    mut frames: mpsc::Receiver<Arc<Vec<u8>>>,
    traffic: Arc<Traffic>,
) {
    let mut socket = None;
    while let Some(frame) = frames.recv().await {
        if socket.is_none() {
            socket = timeout(Duration::from_millis(200), TcpStream::connect(address))
                .await
                .ok()
                .and_then(Result::ok);
            if let Some(stream) = &socket {
                let _ = stream.set_nodelay(true);
            }
        }
        if let Some(stream) = &mut socket
            && matches!(timeout(IO_TIMEOUT, write(stream, &frame)).await, Ok(Ok(())))
        {
            traffic
                .sent
                .fetch_add(4 + frame.len() as u64, Ordering::Relaxed);
        } else {
            socket = None;
            traffic.dropped.fetch_add(1, Ordering::Relaxed);
        }
    }
}

async fn read(stream: &mut TcpStream) -> Result<Vec<u8>> {
    let length = stream.read_u32().await? as usize;
    ensure!(
        (1..=wire::MAX_FRAME).contains(&length),
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
    async fn control_overtakes_application_with_a_bounded_burst() -> Result<()> {
        let (control_tx, control) = mpsc::channel(CONTROL_BURST + 1);
        let (app_tx, application) = mpsc::channel(1);
        let mut inbox = Inbox {
            control,
            application,
            burst: 0,
        };
        let event = |kind| Incoming {
            frame: Frame {
                kind,
                ..Frame::default()
            },
            reply: None,
            submit_received_at: None,
            control_received_at: None,
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
        let (network, _inbox) = Network::listen(0, &[local, peer.local_addr()?], 3).await?;
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
