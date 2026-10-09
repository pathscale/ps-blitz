//! Connection ownership and the six-slot budget for each HTTP origin.

use super::{Connection, Error};
use blitz_traits::net::Url;
use nagoya::sync::{Semaphore, SemaphorePermit};
use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

const PER_ORIGIN_SLOTS: usize = 6;
const IDLE_TIMEOUT: Duration = Duration::from_secs(30);
const MAX_IDLE_ORIGINS: usize = 256;

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub(super) struct Origin {
    pub tls: bool,
    pub host: Arc<str>,
    pub port: u16,
}

impl Origin {
    pub fn from_url(url: &Url) -> Result<Self, Error> {
        let tls = match url.scheme() {
            "https" => true,
            "http" => false,
            scheme => return Err(Error::UnsupportedScheme(scheme.to_owned())),
        };
        let host = url
            .host_str()
            .ok_or(Error::InvalidRequest("URL has no host"))?;
        // URL serialisation includes brackets around IPv6 literals. DNS and
        // rustls ServerName need the address without those brackets.
        let host = host
            .strip_prefix('[')
            .and_then(|host| host.strip_suffix(']'))
            .unwrap_or(host);
        let port = url
            .port_or_known_default()
            .ok_or(Error::InvalidRequest("URL has no port"))?;
        if port == 0 {
            return Err(Error::InvalidRequest("URL port is zero"));
        }
        Ok(Self {
            tls,
            host: Arc::from(host),
            port,
        })
    }

    pub fn host_header(&self) -> String {
        let host = if self.host.contains(':') {
            format!("[{}]", self.host)
        } else {
            self.host.to_string()
        };
        if self.port == if self.tls { 443 } else { 80 } {
            host
        } else {
            format!("{host}:{}", self.port)
        }
    }
}

#[derive(Default)]
pub(super) struct Pool {
    origins: Mutex<HashMap<Origin, Arc<OriginPool>>>,
}

impl Pool {
    pub fn origin(&self, key: Origin) -> Arc<OriginPool> {
        let mut origins = self.origins.lock().expect("HTTP origin pool poisoned");
        if let Some(origin) = origins.get(&key) {
            return origin.clone();
        }
        if origins.len() >= MAX_IDLE_ORIGINS {
            // Keep every active origin and its shared semaphore. Idle origins
            // can be dropped together with their sockets to bound retention.
            origins.retain(|_, origin| Arc::strong_count(origin) > 1);
        }
        origins
            .entry(key)
            .or_insert_with(|| Arc::new(OriginPool::new()))
            .clone()
    }
}

pub(super) struct OriginPool {
    slots: Semaphore,
    idle: Mutex<Vec<Idle>>,
}

struct Idle {
    connection: Connection,
    since: Instant,
}

impl OriginPool {
    fn new() -> Self {
        Self {
            slots: Semaphore::new(PER_ORIGIN_SLOTS),
            idle: Mutex::new(Vec::new()),
        }
    }

    pub async fn acquire(&self) -> Lease<'_> {
        let permit = self.slots.acquire().await;
        let connection = {
            let mut idle = self.idle.lock().expect("HTTP idle pool poisoned");
            idle.retain(|entry| entry.since.elapsed() < IDLE_TIMEOUT);
            idle.pop().map(|entry| entry.connection)
        };
        Lease {
            origin: self,
            _permit: permit,
            connection,
            reusable: false,
        }
    }
}

/// Cancellation drops the connection unless the complete exchange succeeded.
/// The borrowed permit remains held until this lease is dropped.
pub(super) struct Lease<'a> {
    origin: &'a OriginPool,
    _permit: SemaphorePermit<'a>,
    pub connection: Option<Connection>,
    pub reusable: bool,
}

impl Drop for Lease<'_> {
    fn drop(&mut self) {
        if self.reusable
            && let Some(connection) = self.connection.take()
        {
            self.origin
                .idle
                .lock()
                .expect("HTTP idle pool poisoned")
                .push(Idle {
                    connection,
                    since: Instant::now(),
                });
        }
    }
}
