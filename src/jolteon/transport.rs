use std::{
    net::SocketAddr,
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
    time::Duration,
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
const IO_TIMEOUT: Duration = Duration::from_secs(6);

pub struct Incoming {
    pub frame: Frame,
    pub reply: Option<oneshot::Sender<Vec<u8>>>,
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
    peers: Vec<Option<mpsc::Sender<Arc<Vec<u8>>>>>,
}

impl Network {
    pub async fn listen(
        own: u16,
        addresses: &[SocketAddr],
        epoch: u64,
    ) -> Result<(Self, mpsc::Receiver<Incoming>)> {
        let (sender, incoming) = mpsc::channel(256);
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
                let (tx, rx) = mpsc::channel(QUEUE);
                tokio::spawn(send(addr, rx, traffic.clone()));
                Some(tx)
            })
            .collect();
        Ok((
            Self {
                traffic,
                own,
                epoch,
                peers,
            },
            incoming,
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
            self.enqueue(peer, msg.clone());
        }
    }

    pub fn send(&self, peer: u16, kind: u32, body: Vec<u8>) {
        self.send_at(self.epoch, peer, kind, body);
    }

    pub fn send_at(&self, epoch: u64, peer: u16, kind: u32, body: Vec<u8>) {
        if let Some(Some(peer)) = self.peers.get(usize::from(peer)) {
            self.enqueue(peer, self.frame(epoch, kind, body));
        }
    }

    fn enqueue(&self, peer: &mpsc::Sender<Arc<Vec<u8>>>, frame: Arc<Vec<u8>>) {
        if peer.try_send(frame).is_err() {
            self.traffic.dropped.fetch_add(1, Ordering::Relaxed);
        }
    }
}

async fn receive(
    mut stream: TcpStream,
    incoming: mpsc::Sender<Incoming>,
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
                ),
                "unsupported client request"
            );
            let (sender, receiver) = oneshot::channel();
            incoming
                .send(Incoming {
                    frame,
                    reply: Some(sender),
                })
                .await?;
            let response = timeout(IO_TIMEOUT, receiver).await??;
            timeout(IO_TIMEOUT, write(&mut stream, &response)).await??;
            return Ok(());
        }
        ensure!(
            matches!(frame.kind, wire::PROPOSAL..=wire::GET_HANDOFF),
            "unsupported peer frame"
        );
        incoming.send(Incoming { frame, reply: None }).await?;
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
