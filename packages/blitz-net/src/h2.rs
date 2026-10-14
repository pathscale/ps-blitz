//! HTTP/2 over TLS: one multiplexed connection per origin, driven by nagoya.
//!
//! ALPN offers `h2` first. A server that answers `http/1.1` is remembered as
//! HTTP/1.1-only and its connection is handed to the HTTP/1.1 pool. Every new
//! connection still goes through the strict TLS policy in `tls::open`.
//!
//! The `h2` codec is driven through tokio io traits only: no tokio runtime,
//! reactor or timer. Readiness comes from the nagoya reactor, keepalive from
//! nagoya timers.

use super::{Error, MAX_BODY_BYTES, WireResponse, pool::Origin, tls};
use blitz_traits::net::http::{HeaderMap, Method, Request, Uri, Version};
use blitz_traits::platform::{Bytes, FetchRequest, StatusCode};
use bytes::BytesMut;
use h2::client::SendRequest;
use nago_rustls::{
    rustls::ClientConnection,
    rustls_pki_types::{CertificateDer, ServerName},
};
use nagoya::{
    net::TcpStream,
    reactor::{Handle, socket::Addr},
};
use std::{
    collections::{HashMap, HashSet},
    io::{self, Read as _, Write as _},
    pin::Pin,
    sync::{
        Arc, Mutex,
        atomic::{AtomicU64, Ordering},
    },
    task::{Context, Poll, ready},
    time::Duration,
};
use tokio::io::{AsyncRead, AsyncWrite, ReadBuf};

const READ_CHUNK: usize = 16 * 1024;
const STREAM_WINDOW: u32 = 6 * 1024 * 1024;
const CONNECTION_WINDOW: u32 = 15 * 1024 * 1024;
const KEEPALIVE_INTERVAL: Duration = Duration::from_secs(30);
const KEEPALIVE_TIMEOUT: Duration = Duration::from_secs(10);
const IDLE_CLOSE: Duration = Duration::from_secs(300);

/// rustls over a nagoya socket, as tokio io traits for the `h2` codec.
struct TlsIo {
    tcp: TcpStream,
    tls: Box<ClientConnection>,
    /// Plaintext decrypted during the handshake but not read yet.
    early: Vec<u8>,
    early_taken: usize,
    /// Ciphertext produced by rustls, not yet accepted by the socket.
    out: Vec<u8>,
    out_sent: usize,
    close_sent: bool,
    /// Ciphertext read from the socket but not yet taken by rustls.
    cipher: Vec<u8>,
    cipher_taken: usize,
}

fn errno(error: impl std::fmt::Display) -> io::Error {
    io::Error::other(error.to_string())
}

impl TlsIo {
    fn new(tcp: TcpStream, tls: Box<ClientConnection>, early: Vec<u8>) -> Self {
        Self {
            tcp,
            tls,
            early,
            early_taken: 0,
            out: Vec::new(),
            out_sent: 0,
            close_sent: false,
            cipher: Vec::new(),
            cipher_taken: 0,
        }
    }

    /// Send everything rustls has to send. Pending registers the write waker.
    fn poll_drain(&mut self, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        loop {
            if self.out_sent < self.out.len() {
                match ready!(self.tcp.poll_write(cx, &self.out[self.out_sent..])) {
                    Ok(0) => return Poll::Ready(Err(io::ErrorKind::WriteZero.into())),
                    Ok(sent) => self.out_sent += sent,
                    Err(error) => return Poll::Ready(Err(errno(error))),
                }
            } else if self.tls.wants_write() {
                self.out.clear();
                self.out_sent = 0;
                self.tls.write_tls(&mut self.out)?;
            } else {
                return Poll::Ready(Ok(()));
            }
        }
    }
}

impl AsyncRead for TlsIo {
    fn poll_read(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        let this = self.get_mut();
        loop {
            if this.early_taken < this.early.len() {
                let take = (this.early.len() - this.early_taken).min(buf.remaining());
                buf.put_slice(&this.early[this.early_taken..this.early_taken + take]);
                this.early_taken += take;
                if this.early_taken == this.early.len() {
                    this.early = Vec::new();
                    this.early_taken = 0;
                }
                return Poll::Ready(Ok(()));
            }
            match this.tls.reader().read(buf.initialize_unfilled()) {
                Ok(read) => {
                    // Zero is a clean close_notify from the peer.
                    buf.advance(read);
                    return Poll::Ready(Ok(()));
                }
                Err(error) if error.kind() == io::ErrorKind::WouldBlock => {}
                Err(error) => return Poll::Ready(Err(error)),
            }
            // rustls may owe the peer a message on a read, such as a key update.
            if let Poll::Ready(Err(error)) = this.poll_drain(cx) {
                return Poll::Ready(Err(error));
            }
            if this.cipher_taken == this.cipher.len() {
                this.cipher.resize(READ_CHUNK, 0);
                this.cipher_taken = 0;
                let read = match this.tcp.poll_read(cx, &mut this.cipher) {
                    Poll::Ready(Ok(read)) => read,
                    Poll::Ready(Err(error)) => {
                        this.cipher.clear();
                        return Poll::Ready(Err(errno(error)));
                    }
                    Poll::Pending => {
                        // Nothing arrived: the zeroed buffer is not ciphertext.
                        this.cipher.clear();
                        return Poll::Pending;
                    }
                };
                this.cipher.truncate(read);
                if read == 0 {
                    return Poll::Ready(Ok(()));
                }
            }
            // One record batch at a time: read_tls fails with "received
            // plaintext buffer full" if called while undrained plaintext is
            // over its limit, so the rest waits here until the reader above
            // has drained it.
            let mut cursor = &this.cipher[this.cipher_taken..];
            let took = this.tls.read_tls(&mut cursor)?;
            this.cipher_taken += took;
            if let Err(error) = this.tls.process_new_packets() {
                // Send the queued alert before giving up.
                let _ = this.poll_drain(cx);
                return Poll::Ready(Err(io::Error::new(io::ErrorKind::InvalidData, error)));
            }
        }
    }
}

impl AsyncWrite for TlsIo {
    fn poll_write(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        data: &[u8],
    ) -> Poll<io::Result<usize>> {
        let this = self.get_mut();
        loop {
            let took = this.tls.writer().write(data)?;
            if took > 0 || data.is_empty() {
                // Accepted into rustls; a pending socket wakes the flush.
                if let Poll::Ready(Err(error)) = this.poll_drain(cx) {
                    return Poll::Ready(Err(error));
                }
                return Poll::Ready(Ok(took));
            }
            ready!(this.poll_drain(cx))?;
        }
    }

    fn poll_flush(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        let this = self.get_mut();
        this.tls.writer().flush()?;
        this.poll_drain(cx)
    }

    fn poll_shutdown(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        let this = self.get_mut();
        if !this.close_sent {
            this.tls.send_close_notify();
            this.close_sent = true;
        }
        this.poll_drain(cx)
    }
}

/// A live HTTP/2 connection, shared by its origin and any coalesced origins.
struct Live {
    id: u64,
    origin: Origin,
    send: SendRequest<Bytes>,
    peer: Option<Addr>,
    leaf: Option<CertificateDer<'static>>,
    last_used: AtomicU64,
}

#[derive(Default)]
struct State {
    live: HashMap<Origin, Arc<Live>>,
    h1_only: HashSet<Origin>,
    no_coalesce: HashSet<Origin>,
    dialing: HashMap<Origin, Arc<nagoya::sync::Semaphore>>,
}

/// Every HTTP/2 connection of the process, one per origin.
#[derive(Default)]
pub(super) struct H2Pool {
    state: Mutex<State>,
    next_id: AtomicU64,
}

/// What the HTTP/2 pool decided for one request.
pub(super) enum Route {
    H2(WireResponse),
    /// Use HTTP/1.1, with the connection already opened when ALPN chose it.
    Http1(Option<tls::Transport>),
}

fn h2_error(error: h2::Error) -> Error {
    if error.is_io() || error.is_go_away() || error.is_remote() {
        Error::Io(format!("HTTP/2: {error}"))
    } else {
        Error::Write(format!("HTTP/2: {error}"))
    }
}

fn retryable(error: &h2::Error) -> bool {
    error.is_go_away() || error.is_io() || error.reason() == Some(h2::Reason::REFUSED_STREAM)
}

impl H2Pool {
    fn lock(&self) -> std::sync::MutexGuard<'_, State> {
        self.state.lock().expect("HTTP/2 pool poisoned")
    }

    fn remove(&self, id: u64) {
        self.lock().live.retain(|_, live| live.id != id);
    }

    fn current(&self, origin: &Origin) -> Option<Arc<Live>> {
        self.lock().live.get(origin).cloned()
    }

    /// Send `request` over HTTP/2 when the origin speaks it.
    pub async fn route(
        self: &Arc<Self>,
        origin: &Origin,
        request: &FetchRequest,
        headers: &HeaderMap,
        handle: &Handle,
        config: &Arc<nago_rustls::rustls::ClientConfig>,
        addresses: &[Addr],
    ) -> Result<Route, Error> {
        if !origin.tls || self.lock().h1_only.contains(origin) {
            return Ok(Route::Http1(None));
        }
        let idempotent = request.method == Method::GET || request.method == Method::HEAD;
        let mut attempts = 0;
        loop {
            attempts += 1;
            let live = match self.current(origin) {
                Some(live) => live,
                None => match self.dial(origin, handle, config, addresses).await? {
                    Ok(live) => live,
                    Err(transport) => return Ok(Route::Http1(transport)),
                },
            };
            match exchange(&live, request, headers, origin).await {
                Ok(wire)
                    if wire.status == StatusCode::MISDIRECTED_REQUEST && live.origin != *origin =>
                {
                    // The coalesced server will not serve this origin; give it its own.
                    info(format_args!(
                        "net {}: 421 on the connection to {}, coalescing disabled",
                        origin.host, live.origin.host
                    ));
                    let mut state = self.lock();
                    state.no_coalesce.insert(origin.clone());
                    state.live.remove(origin);
                }
                Ok(wire) => {
                    live.last_used.store(nagoya::now_ns(), Ordering::Relaxed);
                    return Ok(Route::H2(wire));
                }
                Err(Failure::Before(error))
                    if attempts < 3 && (idempotent || retryable(&error)) =>
                {
                    self.remove(live.id);
                }
                Err(Failure::Before(error) | Failure::After(error)) => return Err(h2_error(error)),
                Err(Failure::Local(error)) => return Err(error),
            }
        }
    }

    /// Reuse a coalescable connection or open one. `Err` hands back an
    /// HTTP/1.1 transport when the server did not choose `h2`.
    async fn dial(
        self: &Arc<Self>,
        origin: &Origin,
        handle: &Handle,
        config: &Arc<nago_rustls::rustls::ClientConfig>,
        addresses: &[Addr],
    ) -> Result<Result<Arc<Live>, Option<tls::Transport>>, Error> {
        let gate = self
            .lock()
            .dialing
            .entry(origin.clone())
            .or_insert_with(|| Arc::new(nagoya::sync::Semaphore::new(1)))
            .clone();
        let _permit = gate.acquire().await;
        // Whoever held the gate may have connected meanwhile.
        if let Some(live) = self.current(origin) {
            return Ok(Ok(live));
        }
        if self.lock().h1_only.contains(origin) {
            return Ok(Err(None));
        }
        if let Some(live) = self.coalesce(origin, addresses).await {
            return Ok(Ok(live));
        }
        let transport = tls::open(origin, handle, config.clone(), addresses).await?;
        let session = match transport {
            tls::Transport::Tls(session) if session.alpn_protocol() == Some(b"h2") => session,
            other => {
                info(format_args!("net {}: server chose HTTP/1.1", origin.host));
                tls::record_alpn(&origin.host, "http/1.1");
                self.lock().h1_only.insert(origin.clone());
                return Ok(Err(Some(other)));
            }
        };
        tls::record_alpn(&origin.host, "h2");
        let peer = session.get_ref().peer_addr().ok();
        let (tcp, connection, early) = session
            .into_client_parts()
            .map_err(|_| Error::Tls("not a client session".to_owned()))?;
        let leaf = connection
            .peer_certificates()
            .and_then(|chain| chain.first())
            .map(|leaf| leaf.clone().into_owned());
        let (send, mut driver) = h2::client::Builder::new()
            .enable_push(false)
            .initial_window_size(STREAM_WINDOW)
            .initial_connection_window_size(CONNECTION_WINDOW)
            .handshake::<_, Bytes>(TlsIo::new(tcp, connection, early))
            .await
            .map_err(h2_error)?;
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let live = Arc::new(Live {
            id,
            origin: origin.clone(),
            send,
            peer,
            leaf,
            last_used: AtomicU64::new(nagoya::now_ns()),
        });
        info(format_args!(
            "net {}: HTTP/2 connection {id} up",
            origin.host
        ));
        let ping = driver.ping_pong();
        let pool = Arc::downgrade(self);
        let host = origin.host.clone();
        nagoya::spawn(async move {
            let result = driver.await;
            if let Some(pool) = pool.upgrade() {
                pool.remove(id);
            }
            match result {
                Ok(()) => info(format_args!("net {host}: HTTP/2 connection {id} closed")),
                Err(error) => info(format_args!(
                    "net {host}: HTTP/2 connection {id} ended: {error}"
                )),
            }
        });
        if let Some(ping) = ping {
            nagoya::spawn(keepalive(Arc::downgrade(self), Arc::downgrade(&live), ping));
        }
        self.lock().live.insert(origin.clone(), live.clone());
        Ok(Ok(live))
    }

    /// RFC 9113 section 9.1.1: reuse a connection whose certificate covers
    /// `origin` and whose peer address is among the addresses it resolves to.
    async fn coalesce(&self, origin: &Origin, addresses: &[Addr]) -> Option<Arc<Live>> {
        let candidates: Vec<Arc<Live>> = {
            let state = self.lock();
            if state.no_coalesce.contains(origin) {
                return None;
            }
            state
                .live
                .values()
                .filter(|live| live.origin.port == origin.port && live.origin.host != origin.host)
                .cloned()
                .collect()
        };
        if candidates.is_empty() {
            return None;
        }

        let name = ServerName::try_from(origin.host.to_string()).ok()?;
        let live = candidates.into_iter().find(|live| {
            live.peer.is_some_and(|peer| addresses.contains(&peer))
                && live.leaf.as_ref().is_some_and(|leaf| {
                    webpki::EndEntityCert::try_from(leaf)
                        .is_ok_and(|cert| cert.verify_is_valid_for_subject_name(&name).is_ok())
                })
        })?;
        info(format_args!(
            "net {}: coalesced onto HTTP/2 connection {} to {}",
            origin.host, live.id, live.origin.host
        ));
        tls::record_alpn(&origin.host, "h2 (coalesced)");
        self.lock().live.insert(origin.clone(), live.clone());
        Some(live)
    }
}

/// PING on a timer while the connection lives; close it when idle too long.
async fn keepalive(
    pool: std::sync::Weak<H2Pool>,
    live: std::sync::Weak<Live>,
    mut ping: h2::PingPong,
) {
    loop {
        nagoya::sleep(KEEPALIVE_INTERVAL).await;
        let (Some(pool), Some(current)) = (pool.upgrade(), live.upgrade()) else {
            return;
        };
        let idle = nagoya::now_ns().saturating_sub(current.last_used.load(Ordering::Relaxed));
        if u128::from(idle) >= IDLE_CLOSE.as_nanos() {
            // Dropping every sender lets h2 close the connection with GOAWAY.
            pool.remove(current.id);
            return;
        }
        drop(current);
        let alive = matches!(
            nagoya::timeout(KEEPALIVE_TIMEOUT, ping.ping(h2::Ping::opaque())).await,
            Ok(Ok(_))
        );
        if !alive {
            if let Some(current) = live.upgrade() {
                info(format_args!(
                    "net {}: HTTP/2 keepalive failed",
                    current.origin.host
                ));
                pool.remove(current.id);
            }
            return;
        }
    }
}

enum Failure {
    /// The request never reached a stream the server accepted.
    Before(h2::Error),
    After(h2::Error),
    Local(Error),
}

/// Keep the send handle until the complete response has been consumed.
/// Dropping an unfinished exchange queues RST_STREAM CANCEL on the driver.
struct StreamReset {
    stream: h2::SendStream<Bytes>,
    complete: bool,
}

impl Drop for StreamReset {
    fn drop(&mut self) {
        if !self.complete {
            self.stream.send_reset(h2::Reason::CANCEL);
        }
    }
}

async fn exchange(
    live: &Live,
    request: &FetchRequest,
    headers: &HeaderMap,
    origin: &Origin,
) -> Result<WireResponse, Failure> {
    let mut path = request.url.path().to_owned();
    if let Some(query) = request.url.query() {
        path.push(char::from(63));
        path.push_str(query);
    }
    let uri = Uri::builder()
        .scheme("https")
        .authority(origin.host_header())
        .path_and_query(path)
        .build()
        .map_err(|_| Failure::Local(Error::InvalidRequest("URL is not a valid HTTP/2 target")))?;
    let mut head = Request::builder()
        .method(request.method.clone())
        .uri(uri)
        .version(Version::HTTP_2)
        .body(())
        .map_err(|_| Failure::Local(Error::InvalidRequest("invalid HTTP/2 request")))?;
    *head.headers_mut() = headers.clone();
    let body = request.body.clone().filter(|body| !body.is_empty());
    if let Some(body) = &body {
        head.headers_mut()
            .insert("content-length", body.len().into());
    }

    let mut send = live.send.clone().ready().await.map_err(Failure::Before)?;
    let (response, stream) = send
        .send_request(head, body.is_none())
        .map_err(Failure::Before)?;
    let mut stream = StreamReset {
        stream,
        complete: false,
    };
    if let Some(body) = body {
        stream
            .stream
            .send_data(body, true)
            .map_err(Failure::After)?;
    }
    let response = response.await.map_err(|error| {
        if error.reason() == Some(h2::Reason::REFUSED_STREAM) {
            Failure::Before(error)
        } else {
            Failure::After(error)
        }
    })?;
    let (parts, mut recv) = response.into_parts();
    let bodiless = request.method == Method::HEAD
        || parts.status == StatusCode::NO_CONTENT
        || parts.status == StatusCode::NOT_MODIFIED;
    let mut buffer = BytesMut::new();
    while let Some(chunk) = recv.data().await {
        let chunk = chunk.map_err(Failure::After)?;
        let _ = recv.flow_control().release_capacity(chunk.len());
        if buffer.len() + chunk.len() > MAX_BODY_BYTES {
            return Err(Failure::Local(Error::TooLarge));
        }
        request.initiator.charge(chunk.len()).map_err(|reason| Failure::Local(Error::Blocked(reason)))?;
        buffer.extend_from_slice(&chunk);
    }
    let _ = recv.trailers().await.map_err(Failure::After)?;
    stream.complete = true;
    Ok(WireResponse {
        status: parts.status,
        headers: parts.headers,
        body: if bodiless {
            Bytes::new()
        } else {
            buffer.freeze()
        },
        bodiless,
        reusable: true,
    })
}

fn info(message: std::fmt::Arguments<'_>) {
    #[cfg(feature = "tracing")]
    tracing::info!("{message}");
    #[cfg(not(feature = "tracing"))]
    eprintln!("info: {message}");
}
