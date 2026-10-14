use std::io;
use std::net::IpAddr;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use blitz_script::{FetchError, ScriptFetcher};
use nagoya::reactor::{Addr, Reactor, TcpListener, TcpStream, block_on_with};
use url::Url;

const SERVE_TIMEOUT: Duration = Duration::from_secs(30);
const MAX_HEADER_BYTES: usize = 64 * 1024;
const MAX_RESPONSE_BYTES: usize = 1024 * 1024;

struct ModuleRun {
    routes: Vec<(&'static str, String)>,
    requested: Mutex<Vec<String>>,
}

pub struct ModuleServer {
    task: Option<nagoya::JoinHandle<Arc<ModuleRun>>>,
    reactor: Option<Reactor>,
}

impl ModuleServer {
    /// Take the completed job's allocation, then transfer its path list.
    pub fn join(mut self) -> io::Result<Vec<String>> {
        let task = self.task.take().unwrap();
        let run = nagoya::block_on(task)
            .ok_or_else(|| io::Error::other("module server task was cancelled"))?;
        self.reactor.take().unwrap().shutdown();
        let run = Arc::try_unwrap(run)
            .map_err(|_| io::Error::other("completed module job still has an owner"))?;
        run.requested
            .into_inner()
            .map_err(|_| io::Error::other("module paths lock was poisoned"))
    }
}

impl Drop for ModuleServer {
    fn drop(&mut self) {
        if let Some(task) = self.task.take() {
            task.cancel();
        }
        if let Some(reactor) = self.reactor.take() {
            reactor.shutdown();
        }
    }
}

fn socket_error(error: nagoya::reactor::Errno) -> io::Error {
    io::Error::from_raw_os_error(error.0)
}

/// A bounded fixture server using readiness and one absolute timer.
pub fn serve_modules(routes: Vec<(&'static str, String)>) -> (String, ModuleServer) {
    let reactor = Reactor::start().expect("module reactor starts");
    let listener =
        TcpListener::bind(Addr::localhost(0), &reactor.handle()).expect("loopback is available");
    let port = listener.local_addr().expect("socket has an address").port();
    let run = Arc::new(ModuleRun {
        routes,
        requested: Mutex::new(Vec::new()),
    });
    let task = nagoya::runtime::background().spawn(async move {
        let _ = nagoya::time::timeout(SERVE_TIMEOUT, serve(&listener, &run)).await;
        run
    });
    (
        format!("http://127.0.0.1:{port}"),
        ModuleServer {
            task: Some(task),
            reactor: Some(reactor),
        },
    )
}

async fn serve(listener: &TcpListener, run: &Arc<ModuleRun>) -> io::Result<()> {
    loop {
        let (mut stream, _) = listener.accept().await.map_err(socket_error)?;
        let mut head = bytes::BytesMut::with_capacity(MAX_HEADER_BYTES);
        loop {
            if head.windows(4).any(|window| window == b"\r\n\r\n") {
                break;
            }
            if head.len() == MAX_HEADER_BYTES {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "module request head is too large",
                ));
            }
            if stream.read_buf(&mut head).await.map_err(socket_error)? == 0 {
                return Err(io::Error::new(
                    io::ErrorKind::UnexpectedEof,
                    "module request head was incomplete",
                ));
            }
        }
        let head = std::str::from_utf8(&head)
            .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
        let path = head
            .split_whitespace()
            .nth(1)
            .unwrap_or("/")
            .split('?')
            .next()
            .unwrap_or("/");
        let body = run
            .routes
            .iter()
            .find(|(route, _)| *route == path)
            .map(|(_, body)| body.as_str());
        let complete = {
            let mut requested = run.requested.lock().unwrap();
            requested.push(path.to_owned());
            run.routes
                .iter()
                .all(|(route, _)| requested.iter().any(|seen| seen == route))
        };
        let (status, body) = match body {
            Some(body) => ("200 OK", body),
            None => ("404 Not Found", ""),
        };
        let header = format!(
            "HTTP/1.1 {status}\r\nContent-Type: text/javascript\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
            body.len()
        );
        stream
            .write_all_vectored(header.as_bytes(), body.as_bytes())
            .await
            .map_err(socket_error)?;
        if complete {
            return Ok(());
        }
    }
}

/// The synchronous fetcher signature is serviced by a local reactor.
pub struct LoopbackFetcher;

impl ScriptFetcher for LoopbackFetcher {
    fn fetch(&self, url: &Url) -> Result<String, FetchError> {
        if url.scheme() != "http" {
            return Err(FetchError::UnsupportedScheme(url.scheme().to_owned()));
        }
        let host = url.host_str().unwrap_or("127.0.0.1");
        let port = url.port().unwrap_or(80);
        let address = match host.parse::<IpAddr>() {
            Ok(IpAddr::V4(ip)) => Addr::V4(ip.octets(), port),
            Ok(IpAddr::V6(ip)) => Addr::V6(ip.octets(), port),
            Err(_) => {
                return Err(FetchError::InvalidData(
                    "fixture host must be a numeric address".into(),
                ));
            }
        };
        let reactor = Reactor::local().map_err(|error| FetchError::Io(socket_error(error)))?;
        let result: io::Result<String> = block_on_with(&reactor, async {
            nagoya::time::timeout(SERVE_TIMEOUT, async {
                let mut stream = TcpStream::connect(address, &reactor.handle())
                    .await
                    .map_err(socket_error)?;
                let request = format!(
                    "GET {} HTTP/1.1\r\nHost: {host}:{port}\r\nConnection: close\r\n\r\n",
                    url.path()
                );
                stream
                    .write_all(request.as_bytes())
                    .await
                    .map_err(socket_error)?;
                let mut bytes = bytes::BytesMut::with_capacity(MAX_RESPONSE_BYTES);
                loop {
                    if bytes.len() == MAX_RESPONSE_BYTES {
                        return Err(io::Error::new(
                            io::ErrorKind::InvalidData,
                            "module response is too large",
                        ));
                    }
                    if stream.read_buf(&mut bytes).await.map_err(socket_error)? == 0 {
                        break;
                    }
                }
                let response = std::str::from_utf8(&bytes)
                    .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
                let (head, body) = response.split_once("\r\n\r\n").ok_or_else(|| {
                    io::Error::new(io::ErrorKind::InvalidData, "no header terminator")
                })?;
                if !head.starts_with("HTTP/1.1 200") {
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidData,
                        format!(
                            "unexpected status for {url}: {}",
                            head.lines().next().unwrap_or_default()
                        ),
                    ));
                }
                Ok(body.to_owned())
            })
            .await
            .map_err(|_| io::Error::new(io::ErrorKind::TimedOut, "module fetch timed out"))?
        });
        reactor.shutdown();
        result.map_err(FetchError::Io)
    }
}
