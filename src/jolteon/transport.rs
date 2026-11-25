use std::{
    net::SocketAddr,
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
    time::{Duration, Instant},
};

use anyhow::{Context, Result, ensure};
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

    fn frame(&self, epoch: u64, kind: u32, body: Vec<u8>) -> Arc<Vec<u8>> {
        Arc::new(
            Frame {
                sender: u32::from(self.own),
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
        let msg = self.frame(epoch, kind, body);
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

    pub(super) fn relay(&self, routes: &[Option<u16>], body: &[u8]) {
        let mut frame = None;
        for (index, route) in routes.iter().enumerate() {
            if let Some(id) = route
                && !routes[..index].contains(route)
                && let Some(peer) = &self.peers[usize::from(*id)]
            {
                let message = frame
                    .get_or_insert_with(|| self.frame(self.epoch, wire::RELAY_TX, body.to_vec()));
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
    loop {
        let raw = timeout(IO_TIMEOUT, read(&mut stream)).await??;
        traffic
            .received
            .fetch_add(4 + raw.len() as u64, Ordering::Relaxed);
        let frame: Frame = wire::canonical(&raw)?;
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
            let (sender, receiver) = oneshot::channel();
            let submit_received_at = (frame.kind == wire::SUBMIT).then(Instant::now);
            incoming
                .application
                .send(Incoming {
                    frame,
                    reply: Some(sender),
                    submit_received_at,
                })
                .await?;
            let response = timeout(IO_TIMEOUT, receiver).await??;
            timeout(IO_TIMEOUT, write(&mut stream, &response)).await??;
            return Ok(());
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
        queue
            .send(Incoming {
                frame,
                reply: None,
                submit_received_at: None,
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
    stream.write_u32(bytes.len() as u32).await?;
    stream
        .write_all(bytes)
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
            (0, 3, wire::QUORUM, vec![2, 3])
        );
        Ok(())
    }
}
