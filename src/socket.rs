use std::{
    fs,
    io::{self, BufReader, Read, Write},
    net::Shutdown,
    os::unix::{
        fs::{FileTypeExt, PermissionsExt},
        net::{UnixListener, UnixStream},
    },
    path::Path,
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
        mpsc::{self, Receiver, SyncSender},
    },
    thread,
    time::Instant,
};

use anyhow::{Context, Result, ensure};
use prost::Message;
use rayon::{ThreadPool, ThreadPoolBuilder};
use tendermint_proto::v0_38::abci::{Request, Response, request::Value};

pub trait Handler: Send + Sync + 'static {
    fn dispatch(&self, request: Value) -> tendermint_proto::v0_38::abci::response::Value;

    fn record_abci_flush(&self, _received: Instant) {}
}

const MAX_CONNECTIONS: usize = 8;
const MAX_FRAME_BYTES: usize = 32 * 1024 * 1024;
pub const MAX_PENDING_RESPONSES: usize = 128;

pub fn serve(path: &Path, app: impl Handler, workers: usize, response_queue: usize) -> Result<()> {
    match fs::symlink_metadata(path) {
        Ok(metadata) => {
            ensure!(
                metadata.file_type().is_socket(),
                "ABCI path exists and is not a socket"
            );
            match UnixStream::connect(path) {
                Err(error) if error.kind() == io::ErrorKind::ConnectionRefused => {
                    fs::remove_file(path)?
                }
                _ => anyhow::bail!("ABCI socket is already in use"),
            }
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Err(error) => return Err(error.into()),
    }
    let listener = UnixListener::bind(path).with_context(|| format!("bind {}", path.display()))?;
    fs::set_permissions(path, fs::Permissions::from_mode(0o600))?;
    let app = Arc::new(app);
    let admission = Arc::new(
        ThreadPoolBuilder::new()
            .num_threads(workers)
            .thread_name(|id| format!("stem-admit-{id}"))
            .build()?,
    );
    let active = Arc::new(AtomicUsize::new(0));
    eprintln!(
        "level=info event=abci_listening socket={:?} admission_workers={workers} response_queue={response_queue}",
        path
    );
    #[cfg(target_os = "linux")]
    notify_systemd()?;
    for stream in listener.incoming() {
        let stream = stream?;
        if active.load(Ordering::Relaxed) >= MAX_CONNECTIONS {
            continue;
        }
        active.fetch_add(1, Ordering::Relaxed);
        let connections = Arc::clone(&active);
        let app = Arc::clone(&app);
        let admission = Arc::clone(&admission);
        thread::Builder::new()
            .name("stem-abci".into())
            .spawn(move || {
                let _guard = Connection(connections);
                if let Err(error) = handle(stream, app, &admission, response_queue) {
                    eprintln!(
                        "level=warn event=abci_disconnected reason={:?}",
                        error.to_string()
                    );
                }
            })?;
    }
    Ok(())
}

#[cfg(target_os = "linux")]
fn notify_systemd() -> Result<()> {
    use std::os::{
        linux::net::SocketAddrExt,
        unix::{
            ffi::OsStrExt,
            net::{SocketAddr, UnixDatagram},
        },
    };

    let Some(socket) = std::env::var_os("NOTIFY_SOCKET") else {
        return Ok(());
    };
    let addr = if let Some(name) = socket.as_bytes().strip_prefix(b"@") {
        SocketAddr::from_abstract_name(name)?
    } else {
        SocketAddr::from_pathname(Path::new(&socket))?
    };
    UnixDatagram::unbound()?
        .send_to_addr(b"READY=1", &addr)
        .context("notify systemd that ABCI is listening")?;
    Ok(())
}

struct Connection(Arc<AtomicUsize>);

impl Drop for Connection {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::Relaxed);
    }
}

enum Job {
    Immediate(Box<Response>, Option<Instant>),
    Async(Receiver<Response>),
    Barrier(SyncSender<()>),
}

fn handle(
    stream: UnixStream,
    app: Arc<impl Handler>,
    admission: &ThreadPool,
    response_queue: usize,
) -> Result<()> {
    let writer = stream.try_clone()?;
    thread::scope(|scope| {
        let (ordered, queue) = mpsc::sync_channel(response_queue);
        let response_app = Arc::clone(&app);
        let writer = scope.spawn(move || write_responses(writer, queue, response_app.as_ref()));
        let result = read_requests(stream, app, admission, ordered);
        let output = writer.join().expect("ABCI write thread");
        result.and(output)
    })
}

fn read_requests(
    stream: UnixStream,
    app: Arc<impl Handler>,
    admission: &ThreadPool,
    ordered: SyncSender<Job>,
) -> Result<()> {
    let mut reader = BufReader::new(stream);
    let mut has_async = false;
    let enqueue = |job| {
        ordered
            .send(job)
            .map_err(|_| anyhow::anyhow!("ABCI response writer stopped"))
    };
    while let Some(length) = read_length(&mut reader)? {
        let mut buffer = vec![0u8; length];
        reader.read_exact(&mut buffer)?;
        let req = Request::decode(buffer.as_slice())?
            .value
            .context("empty ABCI request")?;
        if let Value::CheckTx(_) = req {
            let (response, ready) = mpsc::sync_channel(1);
            enqueue(Job::Async(ready))?;
            let app = Arc::clone(&app);
            admission.spawn(move || {
                let _ = response.send(Response {
                    value: Some(app.dispatch(req)),
                });
            });
            has_async = true;
        } else {
            let flush_received = matches!(&req, Value::Flush(_)).then(Instant::now);
            // The ordered writer acknowledges a Flush after the earlier replies.
            // Keep reading across it. Requests that access application state
            // still wait for the queued checks before dispatch.
            if has_async && flush_received.is_none() {
                let (complete, wait) = mpsc::sync_channel(1);
                enqueue(Job::Barrier(complete))?;
                wait.recv()?;
                has_async = false;
            }
            enqueue(Job::Immediate(
                Box::new(Response {
                    value: Some(app.dispatch(req)),
                }),
                flush_received,
            ))?;
        }
    }
    Ok(())
}

fn write_responses(
    mut stream: UnixStream,
    queue: impl IntoIterator<Item = Job>,
    app: &impl Handler,
) -> Result<()> {
    let result = (|| -> Result<()> {
        let mut data = Vec::new();
        for job in queue {
            let response = match job {
                Job::Immediate(response, flush_received) => {
                    if let Some(received) = flush_received {
                        app.record_abci_flush(received);
                    }
                    *response
                }
                Job::Async(ready) => ready.recv()?,
                Job::Barrier(complete) => {
                    complete.send(())?;
                    continue;
                }
            };
            data.clear();
            response.encode_length_delimited(&mut data)?;
            stream.write_all(&data)?;
        }
        Ok(())
    })();
    if result.is_err() {
        let _ = stream.shutdown(Shutdown::Both);
    }
    result
}

fn read_length(reader: &mut impl Read) -> Result<Option<usize>> {
    let mut length = 0;
    for shift in (0..28).step_by(7) {
        let mut byte = [0u8];
        if shift == 0 && reader.read(&mut byte)? == 0 {
            return Ok(None);
        }
        if shift != 0 {
            reader.read_exact(&mut byte)?;
        }
        length |= usize::from(byte[0] & 0x7f) << shift;
        if byte[0] & 0x80 == 0 {
            ensure!(
                length <= MAX_FRAME_BYTES,
                "ABCI message exceeds frame limit"
            );
            return Ok(Some(length));
        }
    }
    anyhow::bail!("invalid ABCI message length");
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;
    use tendermint_proto::v0_38::abci::{RequestCheckTx, RequestEcho, RequestFlush, response};

    struct App;

    impl Handler for App {
        fn dispatch(&self, request: Value) -> response::Value {
            use tendermint_proto::v0_38::abci::{ResponseCheckTx, ResponseEcho, ResponseFlush};
            match request {
                Value::CheckTx(_) => response::Value::CheckTx(ResponseCheckTx {
                    gas_wanted: 1,
                    ..Default::default()
                }),
                Value::Echo(request) => response::Value::Echo(ResponseEcho {
                    message: request.message,
                }),
                Value::Flush(_) => response::Value::Flush(ResponseFlush {}),
                _ => unreachable!("unexpected request in transport test"),
            }
        }
    }

    #[test]
    fn flush_drains_responses_in_order_while_later_checks_can_queue() {
        let app = Arc::new(App);
        let pool = ThreadPoolBuilder::new().num_threads(1).build().unwrap();
        let (mut client, server) = UnixStream::pair().unwrap();
        let writer_socket = server.try_clone().unwrap();
        let timeout = Duration::from_secs(3);
        client.set_read_timeout(Some(timeout)).unwrap();

        thread::scope(|scope| {
            let (release, blocked) = mpsc::channel();
            let (ready, waiting) = mpsc::channel();
            pool.spawn(move || {
                let _ = ready.send(());
                let _ = blocked.recv();
            });
            waiting.recv_timeout(timeout).unwrap();

            let (ordered, queue) = mpsc::sync_channel(8);
            let request_app = Arc::clone(&app);
            let admission = &pool;
            let reader =
                scope.spawn(move || read_requests(server, request_app, admission, ordered));
            let check = |index: usize| {
                Value::CheckTx(RequestCheckTx {
                    tx: bytes::Bytes::from(vec![index as u8]),
                    ..Default::default()
                })
            };
            for value in [
                check(0),
                Value::Flush(RequestFlush {}),
                check(1),
                Value::Echo(RequestEcho {
                    message: "ordered".into(),
                }),
            ] {
                let req = Request { value: Some(value) };
                client
                    .write_all(&req.encode_length_delimited_to_vec())
                    .unwrap();
            }
            client.shutdown(Shutdown::Write).unwrap();

            let prefix: Vec<_> = (0..4)
                .map(|_| queue.recv_timeout(timeout).unwrap())
                .collect();
            assert!(matches!(&prefix[0], Job::Async(_)));
            assert!(matches!(&prefix[1], Job::Immediate(_, Some(_))));
            assert!(matches!(&prefix[2], Job::Async(_)));
            assert!(matches!(&prefix[3], Job::Barrier(_)));

            let response_app = Arc::clone(&app);
            let writer = scope.spawn(move || {
                write_responses(
                    writer_socket,
                    prefix.into_iter().chain(queue),
                    response_app.as_ref(),
                )
            });
            release.send(()).unwrap();
            let mut client = BufReader::new(client);
            let mut reply = || {
                let mut bytes = vec![0; read_length(&mut client).unwrap().unwrap()];
                client.read_exact(&mut bytes).unwrap();
                Response::decode(bytes.as_slice()).unwrap().value.unwrap()
            };
            assert!(
                matches!(reply(), response::Value::CheckTx(r) if r.code == 0 && r.gas_wanted > 0)
            );
            assert!(matches!(reply(), response::Value::Flush(_)));
            assert!(
                matches!(reply(), response::Value::CheckTx(r) if r.code == 0 && r.gas_wanted > 0)
            );
            assert!(matches!(reply(), response::Value::Echo(r) if r.message == "ordered"));
            reader.join().unwrap().unwrap();
            writer.join().unwrap().unwrap();
        });
    }
}
