//! RFC 9111 private disk cache for the native HTTP client.
//!
//! Open once per profile, then share the returned Arc with every H1 client.
//! The embedder supplies the profile directory. No global cache path is used.
//! Filesystem operations belong on a nagoya worker, never the reactor thread.
//!
//! Metadata is persisted in WorkTable. Immutable body files are synced before
//! publishing their metadata, and retired only after persistence completes.
//! Version 1 is a new schema. Future row changes require an inline migration.
//!
//! GET and HEAD have separate keys. Authorization, ranges, caller conditions,
//! request bodies and redirected results conservatively bypass storage.
//! Cookie, User-Agent and Accept-Encoding are matched in addition to Vary.
//! Set-Cookie is accepted on live responses but never replayed from storage.

use super::{Error, H1Client, MAX_BODY_BYTES, validate_request};
use blitz_traits::platform::{
    Bytes, FetchRequest, FetchResponse, HeaderMap, Method, StatusCode, Url,
    http::{HeaderValue, header::HeaderName},
};
use nagoya::sync::Semaphore;
use psc_nanoid::{Nanoid, alphabet::Base64UrlAlphabet};
use std::{
    fs::{self, File, OpenOptions},
    io::{self, Read, Write},
    path::{Path, PathBuf},
    sync::{
        Mutex,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
    time::{SystemTime, UNIX_EPOCH},
};
use worktable::prelude::*;
use worktable::worktable;

const MAX_METADATA: usize = 48 * 1024;
const MAX_KEY: usize = 16 * 1024;
const MAX_FIELDS: usize = 1024;
const MAX_HEURISTIC: u64 = 24 * 60 * 60;
const PROFILE_LIMITS: Limits = Limits {
    bytes: 2 * 1024 * 1024 * 1024,
    entries: 2048,
};

type Blob = Vec<u8>;

worktable!(
    name: HttpCacheMeta,
    version: 1,
    persist: true,
    columns: {
        key: String primary_key,
        file: String,
        status: u16,
        headers: Blob,
        variant: Blob,
        stored: u64,
        initial_age: u64,
        body_length: u64,
    },
    config: {
        page_size: 65_535,
    }
);

/// A single-user cache shared by document, subresource and script requests.
///
/// Sharing across profiles is unsupported. Keep one open instance per profile.
pub struct PrivateCache {
    table: HttpCacheMetaWorkTable,
    bodies: PathBuf,
    gate: Semaphore,
    limits: Limits,
    // The gate serializes table, file and usage mutations. The mutex just
    // provides interior mutability; its guard is never held across an await.
    usage: Mutex<Usage>,
    generation: AtomicU64,
    enabled: AtomicBool,
}

struct Limits {
    bytes: u128,
    entries: usize,
}

#[derive(Default)]
struct Usage {
    bytes: u128,
    oldest: BTreeSet<(u64, String)>,
}

impl Usage {
    fn insert(&mut self, order: (u64, String), bytes: u128) {
        if self.oldest.insert(order) {
            self.bytes += bytes;
        }
    }

    fn remove(&mut self, row: &HttpCacheMetaRow) {
        if self.oldest.remove(&(row.stored, row.key.clone())) {
            self.bytes -= entry_bytes(row);
        }
    }
}

fn entry_bytes(row: &HttpCacheMetaRow) -> u128 {
    // Charge the body and metadata payload, excluding WorkTable page overhead.
    u128::from(row.body_length)
        + row.key.len() as u128
        + row.file.len() as u128
        + row.headers.len() as u128
        + row.variant.len() as u128
        + size_of::<u16>() as u128
        + 3 * size_of::<u64>() as u128
}

struct Loaded {
    row: HttpCacheMetaRow,
    response: FetchResponse,
}

impl PrivateCache {
    /// Store under `<profile_directory>/http-cache`.
    ///
    /// Each profile retains up to 2 GiB of body and metadata payloads and
    /// 2,048 entries. The oldest stored entries are evicted to make room.
    /// The embedder may continue without a cache if opening fails.
    pub async fn open(profile_directory: impl AsRef<Path>) -> io::Result<Arc<Self>> {
        Self::open_with_limits(profile_directory.as_ref(), PROFILE_LIMITS).await
    }

    async fn open_with_limits(profile_directory: &Path, limits: Limits) -> io::Result<Arc<Self>> {
        let _ = nagoya::runtime::background();
        let directory = profile_directory.join("http-cache");
        let bodies = directory.join("bodies");
        fs::create_dir_all(&bodies)?;
        let path = directory
            .to_str()
            .ok_or_else(|| io::Error::other("cache path is not UTF-8"))?;
        let config = DiskConfig::new_with_table_name(
            path,
            HttpCacheMetaWorkTable::name_snake_case(),
            HttpCacheMetaWorkTable::version(),
        );
        let engine = HttpCacheMetaPersistenceEngine::new(config)
            .await
            .map_err(storage_error)?;
        let table = HttpCacheMetaWorkTable::load(engine)
            .await
            .map_err(storage_error)?;

        // Interrupted publications can leave immutable files without rows.
        // No requests can use this instance until open returns.
        let mut live = BTreeSet::new();
        let mut usage = Usage::default();
        for row in table.select_all().execute().map_err(storage_error)? {
            usage.insert((row.stored, row.key.clone()), entry_bytes(&row));
            live.insert(row.file);
        }
        for entry in fs::read_dir(&bodies)? {
            let entry = entry?;
            let name = entry.file_name();
            if let Some(name) = name.to_str()
                && valid_file_name(name)
                && !live.contains(name)
            {
                remove_file(&entry.path())?;
            }
        }

        let cache = Self {
            table,
            bodies,
            gate: Semaphore::new(1),
            limits,
            usage: Mutex::new(usage),
            generation: AtomicU64::new(0),
            enabled: AtomicBool::new(true),
        };
        // Apply the quota to profiles created before these limits existed.
        cache.trim(0, 0).await?;
        Ok(Arc::new(cache))
    }

    /// Remove persisted entries and body files, including interrupted writes.
    pub async fn clear(&self) -> io::Result<()> {
        let _permit = self.gate.acquire().await;
        self.generation.fetch_add(1, Ordering::AcqRel);
        self.enabled.store(false, Ordering::Release);
        let rows = self.table.select_all().execute().map_err(storage_error)?;
        for row in rows {
            self.table.delete(row.key).await.map_err(storage_error)?;
        }
        self.table.wait_for_ops().await.map_err(storage_error)?;
        for entry in fs::read_dir(&self.bodies)? {
            let entry = entry?;
            if entry.file_name().to_str().is_some_and(valid_file_name) {
                remove_file(&entry.path())?;
            }
        }
        sync_directory(&self.bodies)?;
        *self.usage.lock().map_err(storage_error)? = Usage::default();
        self.enabled.store(true, Ordering::Release);
        Ok(())
    }

    /// Drain persistence before the embedder shuts down its runtime.
    pub async fn flush(&self) -> io::Result<()> {
        let _permit = self.gate.acquire().await;
        self.table.wait_for_ops().await.map_err(storage_error)
    }

    /// Close after all clients and other cache handles have been dropped.
    pub async fn close(self: Arc<Self>) -> io::Result<()> {
        let cache =
            Arc::try_unwrap(self).map_err(|_| io::Error::other("cache still has live handles"))?;
        cache.table.close().await.map_err(storage_error)
    }

    pub(super) async fn fetch(
        self: &Arc<Self>,
        client: &H1Client,
        request: FetchRequest,
    ) -> Result<FetchResponse, Error> {
        validate_request(&request)?;
        let headers = client.prepare_headers(&request)?;
        let request_policy = Policy::read(&headers);
        let generation = self.generation.load(Ordering::Acquire);
        // A stored response cannot provide an unsent redirect request. Still
        // run invalidation below when a live unsafe request changes a resource.
        let usable = !request.returns_redirects()
            && eligible(&request, &headers)
            && !request_policy.no_store;
        let loaded = if usable {
            match self.load(&request, &headers).await {
                Ok(loaded) => loaded,
                Err(error) => {
                    self.disable(error);
                    None
                }
            }
        } else {
            None
        };

        if let Some(loaded) = &loaded
            && reusable(loaded, &request_policy, SystemTime::now())
        {
            request
                .initiator
                .charge(loaded.response.body.len())
                .map_err(Error::Blocked)?;
            return Ok(aged_response(loaded, SystemTime::now()));
        }

        if request_policy.only_if_cached {
            return Ok(FetchResponse::new(request.url, StatusCode::GATEWAY_TIMEOUT));
        }

        let mut conditional = headers.clone();
        let mut validating = false;
        if let Some(loaded) = &loaded {
            if let Some(etag) = loaded.response.headers.get("etag") {
                conditional.insert("if-none-match", etag.clone());
                validating = true;
            } else if let Some(modified) = loaded.response.headers.get("last-modified") {
                conditional.insert("if-modified-since", modified.clone());
                validating = true;
            }
        }

        let mut started = SystemTime::now();
        let mut response = client
            .fetch_inner(request.clone(), Some(conditional))
            .await?;
        let mut received = SystemTime::now();

        if validating && response.status == StatusCode::NOT_MODIFIED {
            let loaded = loaded.as_ref().expect("validation has a stored response");
            if same_url(&request.url, &response.url)
                && validator_matches(&loaded.response.headers, &response.headers)
            {
                let mut merged = loaded.response.clone();
                // A missing Age on a newly validated response means zero.
                merged.headers.remove("age");
                merged.headers.remove("date");
                for name in response.headers.keys() {
                    if !excluded_header(name.as_str())
                        && name != "content-length"
                        && name != "content-encoding"
                    {
                        merged.headers.remove(name);
                        for value in response.headers.get_all(name).iter() {
                            merged.headers.append(name.clone(), value.clone());
                        }
                    }
                }
                if !merged.headers.contains_key("date") {
                    merged.headers.insert(
                        "date",
                        HeaderValue::from_str(&httpdate::fmt_http_date(received))
                            .expect("an HTTP date is a header value"),
                    );
                }
                let age = initial_age(&merged.headers, started, received);
                self.publish(&request, &headers, &merged, started, received, generation)
                    .await;
                set_age(&mut merged.headers, age);
                request
                    .initiator
                    .charge(merged.body.len())
                    .map_err(Error::Blocked)?;
                return Ok(merged);
            }

            // A redirect or a changed validator cannot validate this entry.
            started = SystemTime::now();
            response = client
                .fetch_inner(request.clone(), Some(headers.clone()))
                .await?;
            received = SystemTime::now();
        }

        if !safe_method(&request.method)
            && (response.status.is_success() || response.status.is_redirection())
        {
            let cache = Arc::clone(self);
            let request = request.clone();
            let response = response.clone();
            // Only a complete response reaches this point. Once admitted,
            // invalidation owns the cache gate until persistence finishes.
            let _ = nagoya::spawn(async move {
                let _permit = cache.gate.acquire().await;
                if let Err(error) = cache.invalidate(&request, &response).await {
                    cache.disable(error);
                }
            })
            .await;
        } else if usable && same_url(&request.url, &response.url) {
            self.publish(&request, &headers, &response, started, received, generation)
                .await;
        }
        Ok(response)
    }

    async fn load(
        &self,
        request: &FetchRequest,
        headers: &HeaderMap,
    ) -> io::Result<Option<Loaded>> {
        let _permit = self.gate.acquire().await;
        if !self.enabled.load(Ordering::Acquire) {
            return Ok(None);
        }
        let Some(row) = self.table.select(key(request)) else {
            return Ok(None);
        };
        let stored_headers = decode_headers(&row.headers)?;
        if fingerprint(headers, &stored_headers)?.as_ref() != Some(&row.variant) {
            return Ok(None);
        }
        if !valid_file_name(&row.file) || row.body_length > MAX_BODY_BYTES as u64 {
            return Err(io::Error::other("invalid cache body metadata"));
        }
        if row.body_length > request.initiator.remaining_bytes() as u64 {
            return Err(io::Error::other("resource-byte-cap"));
        }
        let file = File::open(self.bodies.join(&row.file))?;
        if file.metadata()?.len() != row.body_length {
            return Err(io::Error::other("incomplete cache body"));
        }
        let mut body = Vec::new();
        file.take(MAX_BODY_BYTES as u64 + 1)
            .read_to_end(&mut body)?;
        if body.len() as u64 != row.body_length {
            return Err(io::Error::other("cache body changed while reading"));
        }
        let status = StatusCode::from_u16(row.status).map_err(storage_error)?;
        let response = FetchResponse::new(request.url.clone(), status)
            .headers(stored_headers)
            .body(Bytes::from(body));
        Ok(Some(Loaded { row, response }))
    }

    async fn publish(
        self: &Arc<Self>,
        request: &FetchRequest,
        headers: &HeaderMap,
        response: &FetchResponse,
        started: SystemTime,
        received: SystemTime,
        generation: u64,
    ) {
        let cache = Arc::clone(self);
        let request = request.clone();
        let headers = headers.clone();
        let response = response.clone();
        // Dropping the join handle detaches only this completed-response
        // commit. No network operation is detached by cache publication.
        let _ = nagoya::spawn(async move {
            let _permit = cache.gate.acquire().await;
            if let Err(error) = cache
                .store(&request, &headers, &response, started, received, generation)
                .await
            {
                cache.disable(error);
            }
        })
        .await;
    }

    async fn store(
        &self,
        request: &FetchRequest,
        request_headers: &HeaderMap,
        response: &FetchResponse,
        started: SystemTime,
        received: SystemTime,
        generation: u64,
    ) -> io::Result<()> {
        if !self.enabled.load(Ordering::Acquire)
            || self.generation.load(Ordering::Acquire) != generation
            || request
                .signal
                .as_ref()
                .is_some_and(|signal| signal.aborted())
        {
            return Ok(());
        }

        let key = key(request);
        if key.len() > MAX_KEY {
            return Ok(());
        }
        let policy = Policy::read(&response.headers);
        let variant = fingerprint(request_headers, &response.headers)?;
        let explicit = policy.max_age.is_some() || response.headers.contains_key("expires");
        let cacheable = !policy.no_store
            && response.status.as_u16() >= 200
            && response.status != StatusCode::PARTIAL_CONTENT
            && response.status != StatusCode::NOT_MODIFIED
            && (explicit || heuristic_status(response.status))
            && response.body.len() <= MAX_BODY_BYTES
            && variant.is_some();

        if !cacheable {
            return self.evict(&key).await;
        }

        let mut stored_headers = response.headers.clone();
        strip_hop_headers(&mut stored_headers);
        stored_headers.remove("set-cookie");
        let row = HttpCacheMetaRow {
            key,
            file: Nanoid::<21, Base64UrlAlphabet>::new().to_string(),
            status: response.status.as_u16(),
            headers: encode_headers(&stored_headers)?,
            variant: variant.expect("cacheability checked the variant"),
            stored: seconds(received),
            initial_age: initial_age(&response.headers, started, received),
            body_length: response.body.len() as u64,
        };
        let bytes = entry_bytes(&row);
        if bytes > self.limits.bytes || self.limits.entries == 0 {
            return self.evict(&row.key).await;
        }

        let mut old = self.table.select(row.key.clone());
        if let Some(previous) = &old {
            let used = self.usage.lock().map_err(storage_error)?.bytes;
            if used + bytes > self.limits.bytes {
                // Prefer retiring the superseded entry over unrelated entries.
                // Reserve space before writing, even for a replacement body.
                self.retire(std::slice::from_ref(previous)).await?;
                old = None;
            }
        }
        self.trim(bytes, old.is_none() as usize).await?;

        let path = self.bodies.join(&row.file);
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&path)?;
        let written = file.write_all(&response.body).and_then(|_| file.sync_all());
        drop(file);
        if let Err(error) = written {
            let _ = remove_file(&path);
            return Err(error);
        }
        sync_directory(&self.bodies)?;

        if request
            .signal
            .as_ref()
            .is_some_and(|signal| signal.aborted())
        {
            remove_file(&path)?;
            return Ok(());
        }
        // Publication now runs to completion even if its caller is dropped.
        // Keep the gate and both body files until metadata is durable.
        let order = (row.stored, row.key.clone());
        self.table.upsert(row).await.map_err(storage_error)?;
        self.table.wait_for_ops().await.map_err(storage_error)?;
        if let Some(old) = &old
            && valid_file_name(&old.file)
        {
            remove_file(&self.bodies.join(&old.file))?;
        }
        let mut usage = self.usage.lock().map_err(storage_error)?;
        if let Some(old) = &old {
            usage.remove(old);
        }
        usage.insert(order, bytes);
        Ok(())
    }

    // Only called before sharing the cache or while holding the gate.
    async fn trim(&self, added_bytes: u128, added_entries: usize) -> io::Result<()> {
        let victims = {
            let usage = self.usage.lock().map_err(storage_error)?;
            let mut bytes = usage.bytes + added_bytes;
            let mut entries = usage.oldest.len() + added_entries;
            let mut victims = Vec::new();
            for (_, key) in &usage.oldest {
                if bytes <= self.limits.bytes && entries <= self.limits.entries {
                    break;
                }
                let row = self
                    .table
                    .select(key.clone())
                    .ok_or_else(|| io::Error::other("missing cache entry for quota accounting"))?;
                bytes -= entry_bytes(&row);
                entries -= 1;
                victims.push(row);
            }
            victims
        };
        self.retire(&victims).await
    }

    async fn evict(&self, key: &str) -> io::Result<()> {
        if let Some(row) = self.table.select(key.to_owned()) {
            self.retire(&[row]).await?;
        }
        Ok(())
    }

    async fn retire(&self, rows: &[HttpCacheMetaRow]) -> io::Result<()> {
        if rows.is_empty() {
            return Ok(());
        }
        for row in rows {
            self.table
                .delete(row.key.clone())
                .await
                .map_err(storage_error)?;
        }
        // A crash can leave orphan files, but never rows whose bodies were
        // unlinked before their deletion was durable.
        self.table.wait_for_ops().await.map_err(storage_error)?;
        for row in rows {
            if valid_file_name(&row.file) {
                remove_file(&self.bodies.join(&row.file))?;
            }
        }
        let mut usage = self.usage.lock().map_err(storage_error)?;
        for row in rows {
            usage.remove(row);
        }
        Ok(())
    }

    async fn invalidate(&self, request: &FetchRequest, response: &FetchResponse) -> io::Result<()> {
        if request
            .signal
            .as_ref()
            .is_some_and(|signal| signal.aborted())
        {
            return Ok(());
        }
        self.generation.fetch_add(1, Ordering::AcqRel);
        let mut urls = vec![request.url.clone()];
        if response.url.origin() == request.url.origin() {
            urls.push(response.url.clone());
        }
        for name in ["location", "content-location"] {
            if let Some(value) = response.headers.get(name)
                && let Ok(value) = value.to_str()
                && let Ok(url) = response.url.join(value)
                && url.origin() == request.url.origin()
            {
                urls.push(url);
            }
        }
        for url in urls {
            for method in [Method::GET, Method::HEAD] {
                self.evict(&key(&FetchRequest::get(url.clone()).method(method)))
                    .await?;
            }
        }
        Ok(())
    }

    fn disable(&self, error: io::Error) {
        // Cache failures cannot turn a successful network response into an
        // error. Stop serving entries after any persistence failure.
        self.enabled.store(false, Ordering::Release);
        #[cfg(feature = "tracing")]
        tracing::warn!(error = %error, "HTTP disk cache disabled");
        #[cfg(not(feature = "tracing"))]
        let _ = error;
    }
}

fn eligible(request: &FetchRequest, headers: &HeaderMap) -> bool {
    (request.method == Method::GET || request.method == Method::HEAD)
        && request.body.is_none()
        && key(request).len() <= MAX_KEY
        && ![
            "authorization",
            "proxy-authorization",
            "range",
            "if-range",
            "if-match",
            "if-none-match",
            "if-modified-since",
            "if-unmodified-since",
        ]
        .iter()
        .any(|name| headers.contains_key(*name))
}

fn safe_method(method: &Method) -> bool {
    method == Method::GET
        || method == Method::HEAD
        || method == Method::OPTIONS
        || method == Method::TRACE
}

fn key(request: &FetchRequest) -> String {
    let mut url = request.url.clone();
    url.set_fragment(None);
    format!("{} {}", request.method, url)
}

fn same_url(left: &Url, right: &Url) -> bool {
    let mut left = left.clone();
    let mut right = right.clone();
    left.set_fragment(None);
    right.set_fragment(None);
    left == right
}

fn heuristic_status(status: StatusCode) -> bool {
    matches!(
        status.as_u16(),
        200 | 203 | 204 | 300 | 301 | 308 | 404 | 405 | 410 | 414 | 501
    )
}

#[derive(Default)]
struct Policy {
    no_store: bool,
    no_cache: bool,
    must_revalidate: bool,
    only_if_cached: bool,
    max_age: Option<u64>,
    min_fresh: Option<u64>,
    max_stale: Option<u64>,
}

impl Policy {
    fn read(headers: &HeaderMap) -> Self {
        let mut policy = Self::default();
        for value in headers.get_all("cache-control").iter() {
            let Ok(value) = value.to_str() else {
                policy.no_store = true;
                policy.no_cache = true;
                continue;
            };
            let Some(directives) = directives(value) else {
                policy.no_store = true;
                policy.no_cache = true;
                continue;
            };
            for directive in directives {
                let (name, value) = directive
                    .split_once('=')
                    .map(|(name, value)| (name.trim(), Some(value.trim())))
                    .unwrap_or((directive.trim(), None));
                match name.to_ascii_lowercase().as_str() {
                    "no-store" => policy.no_store = true,
                    // Field-qualified no-cache is conservatively validated
                    // as a whole response. private remains cacheable here.
                    "no-cache" => policy.no_cache = true,
                    "must-revalidate" => policy.must_revalidate = true,
                    "only-if-cached" => policy.only_if_cached = true,
                    "max-age" => set_limit(&mut policy.max_age, delta(value)),
                    "min-fresh" => set_limit(&mut policy.min_fresh, delta(value)),
                    "max-stale" => set_limit(
                        &mut policy.max_stale,
                        value.map(|value| delta(Some(value))).unwrap_or(u64::MAX),
                    ),
                    _ => {}
                }
            }
        }
        if !headers.contains_key("cache-control") {
            for value in headers.get_all("pragma").iter() {
                if value.to_str().ok().is_some_and(|value| {
                    value
                        .split(',')
                        .any(|part| part.trim().eq_ignore_ascii_case("no-cache"))
                }) {
                    policy.no_cache = true;
                }
            }
        }
        policy
    }
}

fn directives(value: &str) -> Option<Vec<&str>> {
    let mut quoted = false;
    let mut escaped = false;
    let mut start = 0;
    let mut parts = Vec::new();
    for (index, byte) in value.bytes().enumerate() {
        if escaped {
            escaped = false;
        } else if quoted && byte == b'\\' {
            escaped = true;
        } else if byte == b'"' {
            quoted = !quoted;
        } else if byte == b',' && !quoted {
            parts.push(&value[start..index]);
            start = index + 1;
        }
    }
    if quoted || escaped {
        return None;
    }
    parts.push(&value[start..]);
    Some(parts)
}

fn delta(value: Option<&str>) -> u64 {
    let Some(value) = value else {
        return 0;
    };
    let value = value
        .strip_prefix('"')
        .and_then(|value| value.strip_suffix('"'))
        .unwrap_or(value);
    if value.is_empty() || !value.bytes().all(|byte| byte.is_ascii_digit()) {
        return 0;
    }
    value.parse().unwrap_or(u64::MAX)
}

fn set_limit(slot: &mut Option<u64>, value: u64) {
    // Duplicated directives are invalid. Choose the restrictive result.
    *slot = Some(if slot.is_some() { 0 } else { value });
}

fn date(headers: &HeaderMap, name: &str) -> Option<SystemTime> {
    let mut values = headers.get_all(name).iter();
    let value = values.next()?;
    if values.next().is_some() {
        return None;
    }
    httpdate::parse_http_date(value.to_str().ok()?).ok()
}

fn seconds(time: SystemTime) -> u64 {
    time.duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

fn initial_age(headers: &HeaderMap, started: SystemTime, received: SystemTime) -> u64 {
    let apparent =
        seconds(received).saturating_sub(seconds(date(headers, "date").unwrap_or(received)));
    let mut ages = headers.get_all("age").iter();
    let age = ages
        .next()
        .map(|value| {
            value
                .to_str()
                .ok()
                .and_then(|value| {
                    (!value.is_empty() && value.bytes().all(|byte| byte.is_ascii_digit()))
                        .then(|| value.parse::<u64>().unwrap_or(u64::MAX))
                })
                .unwrap_or(u64::MAX)
        })
        .unwrap_or(0);
    let age = if ages.next().is_some() { u64::MAX } else { age };
    let delay = received
        .duration_since(started)
        .map(|duration| duration.as_secs())
        .unwrap_or(u64::MAX);
    apparent.max(age.saturating_add(delay))
}

fn current_age(loaded: &Loaded, now: SystemTime) -> u64 {
    loaded.row.initial_age.saturating_add(
        seconds(now)
            .checked_sub(loaded.row.stored)
            .unwrap_or(u64::MAX),
    )
}

fn lifetime(loaded: &Loaded, policy: &Policy) -> u64 {
    if let Some(max_age) = policy.max_age {
        return max_age;
    }
    let headers = &loaded.response.headers;
    let date = date(headers, "date")
        .map(seconds)
        .unwrap_or(loaded.row.stored);
    if headers.contains_key("expires") {
        return self::date(headers, "expires")
            .map(seconds)
            .unwrap_or(0)
            .saturating_sub(date);
    }
    if heuristic_status(loaded.response.status) {
        return self::date(headers, "last-modified")
            .map(|modified| date.saturating_sub(seconds(modified)) / 10)
            .unwrap_or(0)
            .min(MAX_HEURISTIC);
    }
    0
}

fn reusable(loaded: &Loaded, request: &Policy, now: SystemTime) -> bool {
    let response = Policy::read(&loaded.response.headers);
    if request.no_cache || response.no_cache || response.no_store {
        return false;
    }
    let age = current_age(loaded, now);
    if request.max_age.is_some_and(|limit| age > limit) {
        return false;
    }
    let lifetime = lifetime(loaded, &response);
    let min_fresh = request.min_fresh.unwrap_or(0);
    if age.saturating_add(min_fresh) < lifetime {
        return true;
    }
    !response.must_revalidate
        && min_fresh == 0
        && request
            .max_stale
            .is_some_and(|limit| age.saturating_sub(lifetime) <= limit)
}

fn aged_response(loaded: &Loaded, now: SystemTime) -> FetchResponse {
    let mut response = loaded.response.clone();
    set_age(&mut response.headers, current_age(loaded, now));
    response
}

fn set_age(headers: &mut HeaderMap, age: u64) {
    headers.insert(
        "age",
        HeaderValue::from_str(&age.to_string()).expect("an age is decimal"),
    );
}

fn validator_matches(stored: &HeaderMap, updated: &HeaderMap) -> bool {
    if let Some(etag) = updated.get("etag") {
        let Some(old) = stored.get("etag") else {
            return false;
        };
        let weak = |value: &HeaderValue| {
            value
                .as_bytes()
                .strip_prefix(b"W/")
                .unwrap_or(value.as_bytes())
                .to_vec()
        };
        return weak(old) == weak(etag);
    }
    if let Some(modified) = updated.get("last-modified")
        && stored.get("etag").is_none()
    {
        return stored.get("last-modified") == Some(modified);
    }
    true
}

fn excluded_header(name: &str) -> bool {
    matches!(
        name,
        "connection"
            | "keep-alive"
            | "proxy-connection"
            | "proxy-authenticate"
            | "proxy-authorization"
            | "te"
            | "trailer"
            | "transfer-encoding"
            | "upgrade"
            | "set-cookie"
    )
}

fn strip_hop_headers(headers: &mut HeaderMap) {
    let nominated: Vec<HeaderName> = headers
        .get_all("connection")
        .iter()
        .filter_map(|value| value.to_str().ok())
        .flat_map(|value| value.split(','))
        .filter_map(|name| HeaderName::from_bytes(name.trim().as_bytes()).ok())
        .collect();
    for name in nominated {
        headers.remove(name);
    }
    let names: Vec<HeaderName> = headers
        .keys()
        .filter(|name| excluded_header(name.as_str()))
        .cloned()
        .collect();
    for name in names {
        headers.remove(name);
    }
}

fn fingerprint(request: &HeaderMap, response: &HeaderMap) -> io::Result<Option<Blob>> {
    let mut names = vec![
        HeaderName::from_static("cookie"),
        HeaderName::from_static("user-agent"),
        HeaderName::from_static("accept-encoding"),
    ];
    for value in response.get_all("vary").iter() {
        let value = value.to_str().map_err(storage_error)?;
        for name in value
            .split(',')
            .map(str::trim)
            .filter(|name| !name.is_empty())
        {
            if name == "*" {
                return Ok(None);
            }
            names.push(HeaderName::from_bytes(name.as_bytes()).map_err(storage_error)?);
        }
    }
    names.sort_unstable_by(|left, right| left.as_str().cmp(right.as_str()));
    names.dedup();
    if names.len() > MAX_FIELDS {
        return Err(io::Error::other("too many Vary fields"));
    }
    let mut encoded = Vec::new();
    for name in names {
        encode_field(&mut encoded, name.as_str().as_bytes())?;
        let values: Vec<&HeaderValue> = request.get_all(&name).iter().collect();
        encoded.extend_from_slice(&(values.len() as u32).to_be_bytes());
        for value in values {
            encode_field(&mut encoded, value.as_bytes())?;
        }
    }
    if encoded.len() > MAX_METADATA {
        return Err(io::Error::other("cache variant metadata is too large"));
    }
    Ok(Some(encoded))
}

fn encode_headers(headers: &HeaderMap) -> io::Result<Blob> {
    let mut encoded = Vec::new();
    if headers.len() > MAX_FIELDS {
        return Err(io::Error::other("too many cache headers"));
    }
    for (name, value) in headers.iter() {
        encode_field(&mut encoded, name.as_str().as_bytes())?;
        encode_field(&mut encoded, value.as_bytes())?;
    }
    Ok(encoded)
}

fn encode_field(encoded: &mut Blob, value: &[u8]) -> io::Result<()> {
    if value.len() > MAX_METADATA
        || encoded.len().saturating_add(value.len()).saturating_add(4) > MAX_METADATA
    {
        return Err(io::Error::other("cache metadata is too large"));
    }
    encoded.extend_from_slice(&(value.len() as u32).to_be_bytes());
    encoded.extend_from_slice(value);
    Ok(())
}

fn decode_headers(encoded: &[u8]) -> io::Result<HeaderMap> {
    if encoded.len() > MAX_METADATA {
        return Err(io::Error::other("cache metadata exceeds its limit"));
    }
    let mut remaining = encoded;
    let mut headers = HeaderMap::new();
    while !remaining.is_empty() {
        if headers.len() >= MAX_FIELDS {
            return Err(io::Error::other("too many stored headers"));
        }
        let name = HeaderName::from_bytes(decode_field(&mut remaining)?).map_err(storage_error)?;
        let value =
            HeaderValue::from_bytes(decode_field(&mut remaining)?).map_err(storage_error)?;
        headers.append(name, value);
    }
    Ok(headers)
}

fn decode_field<'a>(remaining: &mut &'a [u8]) -> io::Result<&'a [u8]> {
    if remaining.len() < 4 {
        return Err(io::Error::other("truncated cache metadata"));
    }
    let length =
        u32::from_be_bytes(remaining[..4].try_into().expect("four bytes were checked")) as usize;
    *remaining = &remaining[4..];
    if length > remaining.len() {
        return Err(io::Error::other("truncated cache metadata field"));
    }
    let (field, rest) = remaining.split_at(length);
    *remaining = rest;
    Ok(field)
}

fn valid_file_name(name: &str) -> bool {
    name.len() == 21
        && name
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-' || byte == b'_')
}

fn remove_file(path: &Path) -> io::Result<()> {
    match fs::remove_file(path) {
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        result => result,
    }
}

fn sync_directory(path: &Path) -> io::Result<()> {
    #[cfg(unix)]
    File::open(path)?.sync_all()?;
    #[cfg(not(unix))]
    let _ = path;
    Ok(())
}

fn storage_error(error: impl std::fmt::Display) -> io::Error {
    io::Error::other(error.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use blitz_traits::resource_policy::ResourcePolicy;
    use std::{
        io::{BufRead, BufReader},
        net::TcpListener,
        thread,
        time::Duration,
    };

    struct TestProfile(PathBuf);

    impl Drop for TestProfile {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    async fn fetch(
        client: &H1Client,
        origin: &Url,
        path: &str,
        cache_control: Option<&'static str>,
    ) -> Option<usize> {
        let url = origin.join(path).unwrap();
        let mut request = FetchRequest::get(url.clone())
            .initiator(Arc::new(ResourcePolicy::default()).browser(url));
        if let Some(value) = cache_control {
            request
                .headers
                .insert("cache-control", HeaderValue::from_static(value));
        }
        let response = client.fetch(request).await.unwrap();
        match response.status {
            StatusCode::OK => Some(response.body.len()),
            StatusCode::GATEWAY_TIMEOUT => None,
            status => panic!("unexpected HTTP status for {path}: {status}"),
        }
    }

    #[test]
    fn profile_quota_evicts_over_http_and_on_reopen() {
        let scratch = std::env::var_os("BLITZ_CACHE_TEST_DIR")
            .map(PathBuf::from)
            .unwrap_or_else(|| {
                std::env::current_exe()
                    .unwrap()
                    .parent()
                    .unwrap()
                    .to_owned()
            });
        let profile = scratch.join(format!(
            "blitz-cache-test-{}",
            Nanoid::<21, Base64UrlAlphabet>::new()
        ));
        fs::create_dir(&profile).unwrap();
        let profile = TestProfile(profile);
        let file_count = || {
            fs::read_dir(profile.0.join("http-cache/bodies"))
                .unwrap()
                .count()
        };
        let listener = TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0)).unwrap();
        let origin = Url::parse(&format!("http://{}/", listener.local_addr().unwrap())).unwrap();
        let server = thread::spawn(move || {
            for (path, length) in [
                ("a", 0),
                ("b", 0),
                ("c", 0),
                ("d", 0),
                ("d", 4096),
                ("e", 4096),
                ("f", 8192),
                ("g", 0),
                ("h", 7168),
            ] {
                let (mut stream, _) = listener.accept().unwrap();
                stream
                    .set_read_timeout(Some(Duration::from_secs(5)))
                    .unwrap();
                {
                    let mut reader = BufReader::new(&mut stream);
                    let mut line = String::new();
                    reader.read_line(&mut line).unwrap();
                    assert_eq!(line, format!("GET /{path} HTTP/1.1\r\n"));
                    loop {
                        line.clear();
                        assert_ne!(reader.read_line(&mut line).unwrap(), 0);
                        if line == "\r\n" {
                            break;
                        }
                    }
                }
                write!(
                    stream,
                    "HTTP/1.1 200 OK\r\nCache-Control: max-age=600\r\nContent-Length: {length}\r\nConnection: close\r\n\r\n"
                )
                .unwrap();
                stream.write_all(&vec![b'x'; length]).unwrap();
            }
        });

        nagoya::block_on(async {
            let cache_only = Some("only-if-cached");
            let cache = PrivateCache::open_with_limits(
                &profile.0,
                Limits {
                    bytes: 10 * 1024,
                    entries: 3,
                },
            )
            .await
            .unwrap();
            let client = H1Client::new("quota-test", None)
                .unwrap()
                .with_cache(Arc::clone(&cache));

            // Empty bodies exhaust entries well before bytes.
            for path in ["a", "b", "c", "d"] {
                assert_eq!(fetch(&client, &origin, path, None).await, Some(0));
            }
            assert_eq!(fetch(&client, &origin, "a", cache_only).await, None);
            assert_eq!(file_count(), 3);

            // Replacing a key at the entry limit keeps the other two entries.
            assert_eq!(
                fetch(&client, &origin, "d", Some("no-cache")).await,
                Some(4096)
            );
            for (path, length) in [("b", 0), ("c", 0), ("d", 4096)] {
                assert_eq!(
                    fetch(&client, &origin, path, cache_only).await,
                    Some(length)
                );
            }
            assert_eq!(file_count(), 3);

            cache.clear().await.unwrap();
            assert_eq!(file_count(), 0);
            // Two entries would fit by count, but their bodies do not fit.
            assert_eq!(fetch(&client, &origin, "e", None).await, Some(4096));
            assert_eq!(fetch(&client, &origin, "f", None).await, Some(8192));
            assert_eq!(fetch(&client, &origin, "e", cache_only).await, None);
            assert_eq!(fetch(&client, &origin, "f", cache_only).await, Some(8192));
            assert_eq!(file_count(), 1);
            assert_eq!(fetch(&client, &origin, "g", None).await, Some(0));
            assert_eq!(file_count(), 2);
            drop(client);
            cache.close().await.unwrap();

            let cache = PrivateCache::open_with_limits(
                &profile.0,
                Limits {
                    bytes: 6 * 1024,
                    entries: 3,
                },
            )
            .await
            .unwrap();
            let client = H1Client::new("quota-test", None)
                .unwrap()
                .with_cache(Arc::clone(&cache));
            assert_eq!(fetch(&client, &origin, "f", cache_only).await, None);
            // A body larger than the budget is still delivered from the network.
            assert_eq!(fetch(&client, &origin, "h", None).await, Some(7168));
            assert_eq!(fetch(&client, &origin, "h", cache_only).await, None);
            assert_eq!(fetch(&client, &origin, "g", cache_only).await, Some(0));
            assert_eq!(file_count(), 1);
            drop(client);
            cache.close().await.unwrap();
        });
        server.join().unwrap();
    }
}
