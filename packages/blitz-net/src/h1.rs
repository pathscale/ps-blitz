//! Native HTTP/1.1 transport for document and script requests.
//!
//! NET-WIRE attaches this module to `Provider`. The transport accepts already
//! encoded request bodies, leaving form encoding and local URL handling with
//! the provider. HTTP error statuses are responses, not transport errors.
//!
//! Clones share the connection pool. All clients share nagoya's background
//! runtime and one process reactor, so `fetch` also works under
//! `nagoya::block_on` without an entered executor.

#[cfg(feature = "cache")]
#[path = "cache.rs"]
pub mod cache;
#[path = "pool.rs"]
mod pool;
#[path = "tls.rs"]
mod tls;

use blitz_traits::net::{
    CookieJar,
    http::{HeaderMap, HeaderValue, Method, header::HeaderName},
};
use blitz_traits::platform::{Bytes, FetchRequest, FetchResponse, StatusCode};
use bytes::BytesMut;
use nagoya::reactor::Reactor;
use pool::{Origin, Pool};
use std::{
    fmt,
    sync::{Arc, OnceLock},
    time::Duration,
};

const MAX_HEAD: usize = 128 * 1024;
const MAX_LINE: usize = 8 * 1024;
const MAX_HEADERS: usize = 1024;
const MAX_INFORMATIONAL: usize = 16;
const MAX_REDIRECTS: usize = 20;

/// Maximum wire body and maximum body after each decompression stage.
pub const MAX_BODY_BYTES: usize = 1024 * 1024 * 1024;

/// A transport failure. HTTP statuses remain available in `FetchResponse`.
#[derive(Debug)]
pub enum Error {
    Io(String),
    Write(String),
    Dns(String),
    Tls(String),
    InvalidRequest(&'static str),
    Protocol(&'static str),
    UnsupportedScheme(String),
    ClosedBeforeResponse,
    TooLarge,
    TooManyRedirects,
    Timeout,
}

impl fmt::Display for Error {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(detail) => write!(formatter, "HTTP I/O failed: {detail}"),
            Self::Write(detail) => write!(formatter, "HTTP write failed: {detail}"),
            Self::Dns(detail) => write!(formatter, "DNS failed: {detail}"),
            Self::Tls(detail) => write!(formatter, "TLS failed: {detail}"),
            Self::InvalidRequest(detail) => write!(formatter, "invalid request: {detail}"),
            Self::Protocol(detail) => write!(formatter, "invalid HTTP response: {detail}"),
            Self::UnsupportedScheme(scheme) => {
                write!(formatter, "unsupported URL scheme: {scheme}")
            }
            Self::ClosedBeforeResponse => formatter.write_str("peer closed before the response"),
            Self::TooLarge => formatter.write_str("HTTP response exceeds its safety limit"),
            Self::TooManyRedirects => formatter.write_str("too many HTTP redirects"),
            Self::Timeout => formatter.write_str("HTTP request timed out"),
        }
    }
}

impl std::error::Error for Error {}

/// One HTTP/1.1 client shared by document loads and guest fetches.
#[derive(Clone)]
pub struct H1Client {
    pool: Arc<Pool>,
    reactor: &'static Reactor,
    tls_config: Arc<nago_rustls::rustls::ClientConfig>,
    user_agent: HeaderValue,
    cookies: Option<Arc<dyn CookieJar>>,
    timeout: Duration,
    #[cfg(feature = "cache")]
    cache: Option<Arc<cache::PrivateCache>>,
}

impl H1Client {
    /// Use platform certificate verification and the process crypto provider.
    ///
    /// If no provider is installed, TLS configuration installs aws-lc-rs.
    /// An embedder's already installed provider is retained.
    pub fn new(
        user_agent: &str,
        cookies: Option<Arc<dyn CookieJar>>,
    ) -> Result<Self, Error> {
        Self::with_tls_config(user_agent, cookies, tls::default_config()?)
    }

    /// Use an explicit TLS policy, for example an embedder's private roots.
    ///
    /// ALPN is restricted to HTTP/1.1 because this transport cannot speak h2.
    pub fn with_tls_config(
        user_agent: &str,
        cookies: Option<Arc<dyn CookieJar>>,
        config: Arc<nago_rustls::rustls::ClientConfig>,
    ) -> Result<Self, Error> {
        let user_agent = HeaderValue::from_str(user_agent)
            .map_err(|_| Error::InvalidRequest("invalid User-Agent"))?;
        let mut config = (*config).clone();
        config.alpn_protocols = vec![b"http/1.1".to_vec()];
        Ok(Self {
            pool: Arc::new(Pool::default()),
            reactor: process_reactor()?,
            tls_config: Arc::new(config),
            user_agent,
            cookies,
            timeout: Duration::from_secs(120),
            #[cfg(feature = "cache")]
            cache: None,
        })
    }

    /// Set the deadline covering permits, DNS, connections and all redirects.
    pub fn with_timeout(mut self, timeout: Duration) -> Self {
        self.timeout = timeout;
        self
    }

    /// Attach the private cache opened for this embedder's profile.
    ///
    /// Client clones share the same cache. Without this call, requests use
    /// the network and no cache directory is created.
    #[cfg(feature = "cache")]
    pub fn with_cache(mut self, cache: Arc<cache::PrivateCache>) -> Self {
        self.cache = Some(cache);
        self
    }

    /// Fetch an HTTP URL, following redirects and retaining response metadata.
    ///
    /// `data:` and `file:` remain provider operations. Every response head,
    /// including redirects and errors, is delivered to the cookie jar before
    /// any subsequent hop.
    pub async fn fetch(&self, request: FetchRequest) -> Result<FetchResponse, Error> {
        nagoya::timeout(self.timeout, async {
            #[cfg(feature = "cache")]
            if let Some(cache) = &self.cache {
                return cache.fetch(self, request).await;
            }
            self.fetch_inner(request, None).await
        })
        .await
        .map_err(|_| Error::Timeout)?
    }

    async fn fetch_inner(
        &self,
        mut request: FetchRequest,
        mut initial_headers: Option<HeaderMap>,
    ) -> Result<FetchResponse, Error> {
        validate_request(&request)?;
        for hop in 0..=MAX_REDIRECTS {
            let key = Origin::from_url(&request.url)?;
            let origin = self.pool.origin(key.clone());
            let mut lease = origin.acquire().await;
            let mut reused = lease.connection.is_some();
            let headers = match initial_headers.take() {
                Some(headers) => headers,
                None => self.prepare_headers(&request)?,
            };
            let wire = loop {
                if lease.connection.is_none() {
                    let transport = tls::open(
                        &key,
                        &self.reactor.handle(),
                        self.tls_config.clone(),
                    )
                    .await?;
                    lease.connection = Some(Connection::new(transport));
                }
                let result = lease
                    .connection
                    .as_mut()
                    .expect("a connection was installed")
                    .exchange(&request, &headers, &key)
                    .await;
                match result {
                    Err(Error::ClosedBeforeResponse | Error::Write(_))
                        if reused
                            && (request.method == Method::GET
                                || request.method == Method::HEAD) =>
                    {
                        // A server can expire an idle socket before our TTL.
                        // Only replay an idempotent request, and only once.
                        lease.connection.take();
                        reused = false;
                    }
                    result => break result?,
                }
            };

            if let Some(jar) = &self.cookies {
                jar.store_from_response(&request.url, &wire.headers);
            }

            if is_redirect(wire.status)
                && let Some(location) = wire.headers.get("location")
            {
                if hop == MAX_REDIRECTS {
                    return Err(Error::TooManyRedirects);
                }
                let location = location
                    .to_str()
                    .map_err(|_| Error::Protocol("invalid Location"))?;
                let next_url = request
                    .url
                    .join(location)
                    .map_err(|_| Error::Protocol("invalid redirect URL"))?;
                let next_origin = Origin::from_url(&next_url)?;
                if !next_url.username().is_empty() || next_url.password().is_some() {
                    return Err(Error::InvalidRequest("redirect URL contains credentials"));
                }
                if key != next_origin {
                    for name in [
                        "authorization",
                        "proxy-authorization",
                        "cookie",
                        "cookie2",
                        "referer",
                    ] {
                        request.headers.remove(name);
                    }
                }

                let switch_to_get = matches!(wire.status.as_u16(), 301 | 302)
                    && request.method == Method::POST
                    || wire.status == StatusCode::SEE_OTHER
                        && request.method != Method::GET
                        && request.method != Method::HEAD;
                if switch_to_get {
                    request.method = Method::GET;
                    request.body = None;
                    for name in [
                        "content-type",
                        "content-length",
                        "content-encoding",
                        "content-language",
                        "content-location",
                        "content-md5",
                        "digest",
                        "transfer-encoding",
                    ] {
                        request.headers.remove(name);
                    }
                }

                lease.reusable = wire.reusable;
                // Release this origin before acquiring the next hop's permit.
                drop(lease);
                request.url = next_url;
                continue;
            }

            let mut response_headers = wire.headers;
            let body = if wire.bodiless {
                wire.body
            } else {
                decode_body(wire.body, &mut response_headers)?
            };
            // Decoding happens while the permit is held. Cancellation or a
            // decoding error drops the socket rather than pooling it.
            lease.reusable = wire.reusable;
            return Ok(FetchResponse::new(request.url, wire.status)
                .headers(response_headers)
                .body(body));
        }
        Err(Error::TooManyRedirects)
    }

    fn prepare_headers(&self, request: &FetchRequest) -> Result<HeaderMap, Error> {
        let mut headers = request.headers.clone();
        let mut nominated = Vec::new();
        for value in headers.get_all("connection").iter() {
            let value = value
                .to_str()
                .map_err(|_| Error::InvalidRequest("invalid Connection header"))?;
            for name in value.split(',') {
                nominated.push(
                    HeaderName::from_bytes(name.trim().as_bytes())
                        .map_err(|_| Error::InvalidRequest("invalid Connection token"))?,
                );
            }
        }
        for name in nominated {
            headers.remove(name);
        }
        // This transport writes a known-length body without a proxy or
        // protocol upgrade. Caller framing must never reach the wire.
        for name in [
            "connection",
            "keep-alive",
            "proxy-connection",
            "upgrade",
            "te",
            "trailer",
            "transfer-encoding",
            "content-length",
            "expect",
            "host",
        ] {
            headers.remove(name);
        }
        headers.insert("user-agent", self.user_agent.clone());

        #[cfg(feature = "compression")]
        if !headers.contains_key("accept-encoding") && !headers.contains_key("range") {
            headers.insert(
                "accept-encoding",
                HeaderValue::from_static("br, gzip, deflate"),
            );
        }

        if let Some(jar) = &self.cookies {
            headers.remove("cookie");
            if let Some(cookie) = jar.cookies_header(&request.url) {
                headers.insert("cookie", cookie);
            }
        }
        Ok(headers)
    }
}

fn process_reactor() -> Result<&'static Reactor, Error> {
    static REACTOR: OnceLock<Result<Reactor, String>> = OnceLock::new();
    // nagoya owns this OnceLock itself, so clients never start another pool.
    let _ = nagoya::runtime::background();
    match REACTOR.get_or_init(|| Reactor::start().map_err(|error| error.to_string())) {
        Ok(reactor) => Ok(reactor),
        Err(error) => Err(Error::Io(error.clone())),
    }
}

fn validate_request(request: &FetchRequest) -> Result<(), Error> {
    Origin::from_url(&request.url)?;
    if !request.url.username().is_empty() || request.url.password().is_some() {
        return Err(Error::InvalidRequest("URL contains credentials"));
    }
    if request.method == Method::CONNECT || request.headers.contains_key("upgrade") {
        return Err(Error::InvalidRequest("tunnels and upgrades are unsupported"));
    }
    if request.body.as_ref().is_some_and(|body| body.len() > MAX_BODY_BYTES) {
        return Err(Error::TooLarge);
    }
    Ok(())
}

fn is_redirect(status: StatusCode) -> bool {
    matches!(status.as_u16(), 301 | 302 | 303 | 307 | 308)
}

struct WireResponse {
    status: StatusCode,
    headers: HeaderMap,
    body: Bytes,
    bodiless: bool,
    reusable: bool,
}

struct Head {
    status: StatusCode,
    headers: HeaderMap,
    persistent: bool,
}

enum Framing {
    Length(usize),
    Chunked,
    Eof,
}

pub(super) struct Connection {
    transport: tls::Transport,
    buffer: BytesMut,
}

impl Connection {
    fn new(transport: tls::Transport) -> Self {
        Self {
            transport,
            buffer: BytesMut::new(),
        }
    }

    async fn exchange(
        &mut self,
        request: &FetchRequest,
        headers: &HeaderMap,
        origin: &Origin,
    ) -> Result<WireResponse, Error> {
        let close_after = has_token(&request.headers, "connection", "close")?;
        let encoded = encode_request(request, headers, origin, close_after)?;
        self.transport.write_all(&encoded).await?;
        if let Some(body) = &request.body {
            self.transport.write_all(body).await?;
        }

        let mut informational = 0;
        let head = loop {
            let head = self.read_head().await?;
            if head.status == StatusCode::SWITCHING_PROTOCOLS {
                return Err(Error::Protocol("unexpected protocol upgrade"));
            }
            if !head.status.is_informational() {
                break head;
            }
            informational += 1;
            if informational > MAX_INFORMATIONAL {
                return Err(Error::Protocol("too many informational responses"));
            }
        };

        let framing = framing(&head.headers)?;
        let bodiless = request.method == Method::HEAD
            || head.status == StatusCode::NO_CONTENT
            || head.status == StatusCode::NOT_MODIFIED;
        let mut reusable = head.persistent && !close_after;
        let body = if bodiless {
            Bytes::new()
        } else {
            match framing {
                Framing::Length(length) => {
                    if length > MAX_BODY_BYTES {
                        return Err(Error::TooLarge);
                    }
                    self.ensure(length).await?;
                    self.buffer.split_to(length).freeze()
                }
                Framing::Chunked => self.read_chunked().await?,
                Framing::Eof => {
                    reusable = false;
                    loop {
                        if self.buffer.len() > MAX_BODY_BYTES {
                            return Err(Error::TooLarge);
                        }
                        if self.fill().await? == 0 {
                            break;
                        }
                    }
                    self.buffer.split().freeze()
                }
            }
        };
        // There is no pipelining. Unsolicited bytes cannot belong to another
        // request and must not become the next response's status line.
        reusable &= self.buffer.is_empty();
        Ok(WireResponse {
            status: head.status,
            headers: head.headers,
            body,
            bodiless,
            reusable,
        })
    }

    async fn fill(&mut self) -> Result<usize, Error> {
        self.transport.read_into(&mut self.buffer).await
    }

    async fn ensure(&mut self, length: usize) -> Result<(), Error> {
        while self.buffer.len() < length {
            if self.fill().await? == 0 {
                return Err(Error::Protocol("connection closed inside the response"));
            }
        }
        Ok(())
    }

    async fn read_head(&mut self) -> Result<Head, Error> {
        loop {
            if let Some(end) = find(&self.buffer, b"\r\n\r\n") {
                if end + 4 > MAX_HEAD {
                    return Err(Error::TooLarge);
                }
                let raw = self.buffer.split_to(end + 4).freeze();
                return parse_head(&raw);
            }
            if self.buffer.len() >= MAX_HEAD {
                return Err(Error::TooLarge);
            }
            match self.fill().await {
                Ok(0) if self.buffer.is_empty() => return Err(Error::ClosedBeforeResponse),
                Ok(0) => return Err(Error::Protocol("connection closed inside headers")),
                Err(Error::Io(_)) if self.buffer.is_empty() => {
                    return Err(Error::ClosedBeforeResponse);
                }
                result => {
                    result?;
                }
            }
        }
    }

    async fn line(&mut self, limit: usize) -> Result<Bytes, Error> {
        loop {
            if let Some(end) = find(&self.buffer, b"\r\n") {
                if end > limit {
                    return Err(Error::TooLarge);
                }
                let line = self.buffer.split_to(end).freeze();
                let _ = self.buffer.split_to(2);
                return Ok(line);
            }
            if self.buffer.len() > limit {
                return Err(Error::TooLarge);
            }
            if self.fill().await? == 0 {
                return Err(Error::Protocol("connection closed inside chunk framing"));
            }
        }
    }

    async fn read_chunked(&mut self) -> Result<Bytes, Error> {
        let mut body = BytesMut::new();
        loop {
            let line = self.line(MAX_LINE).await?;
            let digits = line.split(|byte| *byte == b';').next().unwrap_or_default();
            if digits.is_empty() || !digits.iter().all(u8::is_ascii_hexdigit) {
                return Err(Error::Protocol("invalid chunk size"));
            }
            let digits = std::str::from_utf8(digits)
                .map_err(|_| Error::Protocol("invalid chunk size"))?;
            let size = usize::from_str_radix(digits, 16)
                .map_err(|_| Error::Protocol("invalid chunk size"))?;
            if size == 0 {
                self.read_trailers().await?;
                return Ok(body.freeze());
            }
            if size > MAX_BODY_BYTES.saturating_sub(body.len()) {
                return Err(Error::TooLarge);
            }
            let mut remaining = size;
            while remaining != 0 {
                if self.buffer.is_empty() && self.fill().await? == 0 {
                    return Err(Error::Protocol("connection closed inside a chunk"));
                }
                let take = remaining.min(self.buffer.len());
                let part = self.buffer.split_to(take);
                body.extend_from_slice(&part);
                remaining -= take;
            }
            self.ensure(2).await?;
            if &self.buffer[..2] != b"\r\n" {
                return Err(Error::Protocol("missing chunk terminator"));
            }
            let _ = self.buffer.split_to(2);
        }
    }

    async fn read_trailers(&mut self) -> Result<(), Error> {
        let mut total = 0;
        let mut count = 0;
        loop {
            let line = self.line(MAX_LINE).await?;
            total += line.len() + 2;
            if total > MAX_HEAD {
                return Err(Error::TooLarge);
            }
            if line.is_empty() {
                return Ok(());
            }
            count += 1;
            if count > MAX_HEADERS {
                return Err(Error::TooLarge);
            }
            let (name, _) = parse_header(&line)?;
            if matches!(
                name.as_str(),
                "content-length" | "transfer-encoding" | "host" | "connection" | "trailer"
            ) {
                return Err(Error::Protocol("framing field in trailers"));
            }
            // Consume trailers completely, but do not treat them as response
            // headers or allow them to update cookies after the response head.
        }
    }
}

fn encode_request(
    request: &FetchRequest,
    headers: &HeaderMap,
    origin: &Origin,
    close_after: bool,
) -> Result<Bytes, Error> {
    let mut encoded = BytesMut::new();
    encoded.extend_from_slice(request.method.as_str().as_bytes());
    encoded.extend_from_slice(b" ");
    let path = request.url.path();
    encoded.extend_from_slice(if path.is_empty() { b"/" } else { path.as_bytes() });
    if let Some(query) = request.url.query() {
        encoded.extend_from_slice(b"?");
        encoded.extend_from_slice(query.as_bytes());
    }
    encoded.extend_from_slice(b" HTTP/1.1\r\nHost: ");
    encoded.extend_from_slice(origin.host_header().as_bytes());
    encoded.extend_from_slice(if close_after {
        b"\r\nConnection: close\r\n"
    } else {
        b"\r\nConnection: keep-alive\r\n"
    });
    for (name, value) in headers.iter() {
        encoded.extend_from_slice(name.as_str().as_bytes());
        encoded.extend_from_slice(b": ");
        encoded.extend_from_slice(value.as_bytes());
        encoded.extend_from_slice(b"\r\n");
        if encoded.len() > MAX_HEAD {
            return Err(Error::InvalidRequest("request headers are too large"));
        }
    }
    if let Some(body) = &request.body {
        encoded.extend_from_slice(b"Content-Length: ");
        encoded.extend_from_slice(body.len().to_string().as_bytes());
        encoded.extend_from_slice(b"\r\n");
    }
    encoded.extend_from_slice(b"\r\n");
    if encoded.len() > MAX_HEAD {
        return Err(Error::InvalidRequest("request headers are too large"));
    }
    Ok(encoded.freeze())
}

fn parse_head(raw: &[u8]) -> Result<Head, Error> {
    for (index, byte) in raw.iter().enumerate() {
        if *byte == b'\n' && (index == 0 || raw[index - 1] != b'\r')
            || *byte == b'\r' && raw.get(index + 1) != Some(&b'\n')
        {
            return Err(Error::Protocol("headers require CRLF line endings"));
        }
    }
    let mut lines = raw.split(|byte| *byte == b'\n');
    let status_line = lines
        .next()
        .and_then(|line| line.strip_suffix(b"\r"))
        .ok_or(Error::Protocol("missing status line"))?;
    let mut fields = status_line.splitn(3, |byte| *byte == b' ');
    let version = fields.next().unwrap_or_default();
    let code = fields.next().unwrap_or_default();
    let reason = fields.next().ok_or(Error::Protocol("invalid status line"))?;
    if !matches!(version, b"HTTP/1.1" | b"HTTP/1.0")
        || code.len() != 3
        || !code.iter().all(u8::is_ascii_digit)
        || reason.iter().any(|byte| *byte < b' ' && *byte != b'\t' || *byte == 127)
    {
        return Err(Error::Protocol("invalid status line"));
    }
    let status = StatusCode::from_bytes(code)
        .map_err(|_| Error::Protocol("invalid status code"))?;
    let mut headers = HeaderMap::new();
    let mut count = 0;
    for line in lines {
        let line = line
            .strip_suffix(b"\r")
            .ok_or(Error::Protocol("invalid header line"))?;
        if line.is_empty() {
            break;
        }
        count += 1;
        if count > MAX_HEADERS {
            return Err(Error::TooLarge);
        }
        let (name, value) = parse_header(line)?;
        headers.append(name, value);
    }
    let close = has_token(&headers, "connection", "close")?;
    let persistent = !close
        && (version == b"HTTP/1.1" || has_token(&headers, "connection", "keep-alive")?);
    Ok(Head {
        status,
        headers,
        persistent,
    })
}

fn parse_header(line: &[u8]) -> Result<(HeaderName, HeaderValue), Error> {
    let colon = line
        .iter()
        .position(|byte| *byte == b':')
        .ok_or(Error::Protocol("header has no colon"))?;
    let name = HeaderName::from_bytes(&line[..colon])
        .map_err(|_| Error::Protocol("invalid header name"))?;
    let value = HeaderValue::from_bytes(trim_ows(&line[colon + 1..]))
        .map_err(|_| Error::Protocol("invalid header value"))?;
    Ok((name, value))
}

fn trim_ows(mut bytes: &[u8]) -> &[u8] {
    while bytes.first().is_some_and(|byte| matches!(byte, b' ' | b'\t')) {
        bytes = &bytes[1..];
    }
    while bytes.last().is_some_and(|byte| matches!(byte, b' ' | b'\t')) {
        bytes = &bytes[..bytes.len() - 1];
    }
    bytes
}

fn has_token(headers: &HeaderMap, name: &str, wanted: &str) -> Result<bool, Error> {
    let mut found = false;
    for value in headers.get_all(name).iter() {
        let value = value
            .to_str()
            .map_err(|_| Error::Protocol("invalid header token list"))?;
        found |= value
            .split(',')
            .any(|token| token.trim().eq_ignore_ascii_case(wanted));
    }
    Ok(found)
}

fn framing(headers: &HeaderMap) -> Result<Framing, Error> {
    if headers.contains_key("transfer-encoding") {
        if headers.contains_key("content-length") {
            return Err(Error::Protocol("both Transfer-Encoding and Content-Length"));
        }
        let mut count = 0;
        for value in headers.get_all("transfer-encoding").iter() {
            let value = value
                .to_str()
                .map_err(|_| Error::Protocol("invalid Transfer-Encoding"))?;
            for token in value.split(',') {
                if !token.trim().eq_ignore_ascii_case("chunked") {
                    return Err(Error::Protocol("unsupported transfer coding"));
                }
                count += 1;
            }
        }
        if count != 1 {
            return Err(Error::Protocol("invalid chunked transfer coding"));
        }
        return Ok(Framing::Chunked);
    }
    let mut length = None;
    for value in headers.get_all("content-length").iter() {
        let value = value
            .to_str()
            .map_err(|_| Error::Protocol("invalid Content-Length"))?;
        for field in value.split(',') {
            let field = field.trim();
            if field.is_empty() || !field.as_bytes().iter().all(u8::is_ascii_digit) {
                return Err(Error::Protocol("invalid Content-Length"));
            }
            let parsed = field
                .parse::<usize>()
                .map_err(|_| Error::Protocol("Content-Length overflow"))?;
            if length.is_some_and(|previous| previous != parsed) {
                return Err(Error::Protocol("conflicting Content-Length values"));
            }
            length = Some(parsed);
        }
    }
    Ok(length.map_or(Framing::Eof, Framing::Length))
}

fn find(bytes: &[u8], needle: &[u8]) -> Option<usize> {
    bytes.windows(needle.len()).position(|window| window == needle)
}

#[cfg(not(feature = "compression"))]
fn decode_body(body: Bytes, _headers: &mut HeaderMap) -> Result<Bytes, Error> {
    Ok(body)
}

#[cfg(feature = "compression")]
fn decode_body(mut body: Bytes, headers: &mut HeaderMap) -> Result<Bytes, Error> {
    let mut encodings = Vec::new();
    for value in headers.get_all("content-encoding").iter() {
        let value = value
            .to_str()
            .map_err(|_| Error::Protocol("invalid Content-Encoding"))?;
        for encoding in value.split(',') {
            let encoding = encoding.trim().to_ascii_lowercase();
            if !matches!(encoding.as_str(), "identity" | "br" | "gzip" | "deflate") {
                return Err(Error::Protocol("unsupported content coding"));
            }
            encodings.push(encoding);
        }
    }
    if encodings.is_empty() || encodings.iter().all(|encoding| encoding == "identity") {
        return Ok(body);
    }
    for encoding in encodings.iter().rev() {
        body = match encoding.as_str() {
            "identity" => body,
            "br" => bounded_decode(brotli::Decompressor::new(body.as_ref(), 16 * 1024))?,
            "gzip" => bounded_decode(flate2::read::MultiGzDecoder::new(body.as_ref()))?,
            "deflate" => {
                // HTTP deflate is zlib wrapped, but older servers send raw
                // DEFLATE. Recognise a zlib header without retrying corrupt
                // wrapped data as a different format.
                let wrapped = body.len() >= 2
                    && body[0] & 0x0f == 8
                    && body[0] >> 4 <= 7
                    && u16::from_be_bytes([body[0], body[1]]) % 31 == 0;
                if wrapped {
                    bounded_decode(flate2::read::ZlibDecoder::new(body.as_ref()))?
                } else {
                    bounded_decode(flate2::read::DeflateDecoder::new(body.as_ref()))?
                }
            }
            _ => return Err(Error::Protocol("unsupported content coding")),
        };
    }
    // These described the encoded representation, not the returned bytes.
    headers.remove("content-encoding");
    headers.remove("content-length");
    Ok(body)
}

#[cfg(feature = "compression")]
fn bounded_decode(reader: impl std::io::Read) -> Result<Bytes, Error> {
    use std::io::Read;
    let mut output = Vec::new();
    reader
        .take(MAX_BODY_BYTES as u64 + 1)
        .read_to_end(&mut output)
        .map_err(|error| Error::Io(error.to_string()))?;
    if output.len() > MAX_BODY_BYTES {
        return Err(Error::TooLarge);
    }
    Ok(Bytes::from(output))
}
