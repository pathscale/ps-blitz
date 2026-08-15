//! The semantics, asserted against stub providers.
//!
//! Nothing here touches a network or a disk, because nothing in this crate
//! can. What is being tested is the part this crate owns: origin scoping, the
//! in-flight table, and the rule that a completion is only ever observed by
//! draining.

use super::*;
use blitz_traits::platform::{Bytes, Method, StatusCode, Url};

fn url(text: &str) -> Url {
    Url::parse(text).unwrap()
}

fn host_at(origin: &str, storage: Arc<dyn StorageProvider>) -> PlatformHost {
    PlatformHost::new(
        OriginKey::for_document(&url(origin)),
        Arc::new(blitz_traits::platform::DummyFetchProvider),
        storage,
    )
}

// -- storage --------------------------------------------------------------

#[test]
fn a_value_survives_get_after_set() {
    let host = host_at("https://example.com/", Arc::new(MemoryStorage::new()));
    assert_eq!(host.storage_get("theme"), None);

    host.storage_set("theme", "dark").unwrap();
    assert_eq!(host.storage_get("theme"), Some("dark".to_owned()));

    host.storage_set("theme", "light").unwrap();
    assert_eq!(host.storage_get("theme"), Some("light".to_owned()));

    host.storage_remove("theme");
    assert_eq!(host.storage_get("theme"), None);
}

#[test]
fn removing_what_is_not_there_is_not_an_error() {
    let host = host_at("https://example.com/", Arc::new(MemoryStorage::new()));
    host.storage_remove("never-set");
    host.storage_clear();
    assert_eq!(host.storage_get("never-set"), None);
}

#[test]
fn clear_empties_this_origin() {
    let host = host_at("https://example.com/", Arc::new(MemoryStorage::new()));
    host.storage_set("a", "1").unwrap();
    host.storage_set("b", "2").unwrap();
    host.storage_clear();
    assert_eq!(host.storage_get("a"), None);
    assert_eq!(host.storage_get("b"), None);
}

/// The security property. One store, two origins, no leakage in either
/// direction, and a `clear` on one that does not touch the other.
#[test]
fn two_origins_sharing_one_store_cannot_see_each_other() {
    let store = Arc::new(MemoryStorage::new());
    let one = host_at("https://one.example/", store.clone());
    let two = host_at("https://two.example/", store.clone());

    one.storage_set("token", "one-secret").unwrap();
    two.storage_set("token", "two-secret").unwrap();

    assert_eq!(one.storage_get("token"), Some("one-secret".to_owned()));
    assert_eq!(two.storage_get("token"), Some("two-secret".to_owned()));
    assert_eq!(store.origin_count(), 2);

    one.storage_clear();
    assert_eq!(one.storage_get("token"), None);
    assert_eq!(
        two.storage_get("token"),
        Some("two-secret".to_owned()),
        "clearing one origin must not touch another"
    );
}

/// A port is part of an origin, so a development server on 8080 does not read
/// what the one on 3000 wrote.
#[test]
fn a_different_port_is_a_different_origin() {
    let store = Arc::new(MemoryStorage::new());
    let three_thousand = host_at("http://localhost:3000/", store.clone());
    let eight_thousand = host_at("http://localhost:8080/", store.clone());

    three_thousand.storage_set("session", "abc").unwrap();
    assert_eq!(eight_thousand.storage_get("session"), None);
}

/// Two local files are two origins. Collapsing them would let any HTML file on
/// the disk read any other's saved state.
#[test]
fn two_file_documents_do_not_share_storage() {
    let store = Arc::new(MemoryStorage::new());
    let one = host_at("file:///home/user/one.html", store.clone());
    let two = host_at("file:///home/user/two.html", store.clone());

    one.storage_set("note", "private").unwrap();
    assert_eq!(two.storage_get("note"), None);
    assert!(!one.origin().is_persistable());
}

// -- fetch ----------------------------------------------------------------

type Answer = Box<dyn Fn(&FetchRequest) -> Result<FetchResponse, FetchError> + Send + Sync>;

/// Answers the moment it is asked, on the calling thread.
struct Immediate(Answer);

impl FetchProvider for Immediate {
    fn fetch(&self, request: FetchRequest, handler: Box<dyn FetchHandler>) {
        let result = (self.0)(&request);
        handler.complete(result);
    }
}

/// Keeps the handler so a test can decide when the answer arrives.
#[derive(Default)]
struct Deferred {
    handlers: Mutex<Vec<Box<dyn FetchHandler>>>,
}

impl Deferred {
    fn answer_all(&self, result: impl Fn() -> Result<FetchResponse, FetchError>) {
        for handler in self.handlers.lock().unwrap().drain(..) {
            handler.complete(result());
        }
    }
}

impl FetchProvider for Deferred {
    fn fetch(&self, _request: FetchRequest, handler: Box<dyn FetchHandler>) {
        self.handlers.lock().unwrap().push(handler);
    }
}

fn ok_response(body: &'static str) -> Result<FetchResponse, FetchError> {
    Ok(
        FetchResponse::new(url("https://example.com/thing"), StatusCode::OK)
            .body(Bytes::from_static(body.as_bytes())),
    )
}

fn fetch_host(provider: Arc<dyn FetchProvider>) -> PlatformHost {
    PlatformHost::new(
        OriginKey::for_document(&url("https://example.com/")),
        provider,
        Arc::new(MemoryStorage::new()),
    )
}

#[test]
fn a_completed_request_is_drained_then_read() {
    let host = fetch_host(Arc::new(Immediate(Box::new(|_| ok_response("hello")))));
    let id = host.start_fetch(FetchRequest::get(url("https://example.com/thing")));

    assert_eq!(host.take_ready(), vec![id]);
    assert_eq!(host.state(id), FetchState::Response);

    let body = host
        .with_response(id, |response| response.body.to_vec())
        .expect("a response");
    assert_eq!(body, b"hello");

    assert!(host.release(id));
    assert_eq!(
        host.state(id),
        FetchState::Unknown,
        "a released id must not name anything"
    );
}

/// Draining is a *take*: a response is reported ready exactly once, so a
/// binding cannot dispatch the same completion to a guest twice.
#[test]
fn draining_twice_yields_the_completion_once() {
    let host = fetch_host(Arc::new(Immediate(Box::new(|_| ok_response("hello")))));
    let id = host.start_fetch(FetchRequest::get(url("https://example.com/thing")));

    assert_eq!(host.take_ready(), vec![id]);
    assert!(host.take_ready().is_empty());
    assert_eq!(
        host.state(id),
        FetchState::Response,
        "draining reports the completion, it does not consume the response"
    );
}

/// The reason this trait exists at all. `NetProvider` turns a 404 into an error
/// it logs and drops; here it is an ordinary response with a status.
#[test]
fn a_404_is_a_response_and_not_a_failure() {
    let host = fetch_host(Arc::new(Immediate(Box::new(|_| {
        Ok(
            FetchResponse::new(url("https://example.com/missing"), StatusCode::NOT_FOUND)
                .body(Bytes::from_static(b"<h1>Not Found</h1>")),
        )
    }))));

    let id = host.start_fetch(FetchRequest::get(url("https://example.com/missing")));
    host.take_ready();

    assert_eq!(host.state(id), FetchState::Response);
    let status = host.with_response(id, |r| r.status).unwrap();
    assert_eq!(status, StatusCode::NOT_FOUND);
    let body_len = host.with_response(id, |r| r.body.len()).unwrap();
    assert_eq!(body_len, 18);
}

#[test]
fn a_request_that_cannot_be_completed_reports_why() {
    let host = fetch_host(Arc::new(Immediate(Box::new(|_| {
        Err(FetchError::Network("dns failure".to_owned()))
    }))));

    let id = host.start_fetch(FetchRequest::get(url("https://nowhere.invalid/")));
    assert_eq!(host.take_ready(), vec![id]);
    assert_eq!(
        host.state(id),
        FetchState::Failed(FetchError::Network("dns failure".to_owned()))
    );
    assert!(
        host.with_response(id, |_| ()).is_none(),
        "a failure has no response to read"
    );
}

#[test]
fn a_request_is_pending_until_the_provider_answers() {
    let provider = Arc::new(Deferred::default());
    let host = fetch_host(provider.clone());

    let id = host.start_fetch(FetchRequest::get(url("https://example.com/slow")));
    assert_eq!(host.state(id), FetchState::Pending);
    assert!(host.take_ready().is_empty());

    provider.answer_all(|| ok_response("late"));
    assert_eq!(host.take_ready(), vec![id]);
}

/// Releasing a request in flight must not be resurrected by its own answer
/// arriving afterwards. Without this, tearing down a page with a request
/// outstanding leaves an entry nothing will ever drain.
#[test]
fn a_late_answer_to_a_released_request_is_dropped() {
    let provider = Arc::new(Deferred::default());
    let host = fetch_host(provider.clone());

    let id = host.start_fetch(FetchRequest::get(url("https://example.com/slow")));
    assert!(host.release(id));
    assert_eq!(host.tracked_requests(), 0);

    provider.answer_all(|| ok_response("too late"));

    assert_eq!(host.state(id), FetchState::Unknown);
    assert!(host.take_ready().is_empty());
    assert_eq!(host.tracked_requests(), 0);
}

/// Dropping the whole host with a request outstanding, then answering it. The
/// handler holds a `Weak`, so this is a no-op rather than a panic or a leak.
#[test]
fn answering_after_the_host_is_gone_does_nothing() {
    let provider = Arc::new(Deferred::default());
    {
        let host = fetch_host(provider.clone());
        host.start_fetch(FetchRequest::get(url("https://example.com/slow")));
    }
    provider.answer_all(|| ok_response("nobody home"));
}

#[test]
fn the_waker_fires_once_per_completion() {
    use std::sync::atomic::{AtomicUsize, Ordering};

    let provider = Arc::new(Deferred::default());
    let wakes = Arc::new(AtomicUsize::new(0));
    let counter = wakes.clone();

    let host = PlatformHost::new(
        OriginKey::for_document(&url("https://example.com/")),
        provider.clone(),
        Arc::new(MemoryStorage::new()),
    )
    .with_waker(Arc::new(move || {
        counter.fetch_add(1, Ordering::SeqCst);
    }));

    host.start_fetch(FetchRequest::get(url("https://example.com/a")));
    host.start_fetch(FetchRequest::get(url("https://example.com/b")));
    assert_eq!(wakes.load(Ordering::SeqCst), 0);

    provider.answer_all(|| ok_response("x"));
    assert_eq!(wakes.load(Ordering::SeqCst), 2);
}

#[test]
fn ids_are_not_reused_after_release() {
    let host = fetch_host(Arc::new(Immediate(Box::new(|_| ok_response("x")))));

    let first = host.start_fetch(FetchRequest::get(url("https://example.com/a")));
    host.take_ready();
    host.release(first);

    let second = host.start_fetch(FetchRequest::get(url("https://example.com/b")));
    assert_ne!(first, second);
}

// -- counters -------------------------------------------------------------

#[test]
fn counters_separate_what_was_sent_from_what_came_back() {
    let host = fetch_host(Arc::new(Immediate(Box::new(|_| ok_response("0123456789")))));

    host.start_fetch(
        FetchRequest::get(url("https://example.com/thing"))
            .method(Method::POST)
            .body(Bytes::from_static(b"abc")),
    );
    host.take_ready();

    let counters = host.counters();
    assert_eq!(counters.fetches_started, 1);
    assert_eq!(counters.fetches_completed, 1);
    assert_eq!(counters.fetch_bytes_sent, 3);
    assert_eq!(counters.fetch_bytes_received, 10);
}

/// Reading a body twice moves twice the bytes across a *guest* boundary and the
/// same bytes across this one. This asserts the split the counters module
/// describes: this number does not move on a re-read.
#[test]
fn a_second_read_of_the_same_body_is_not_counted_again() {
    let host = fetch_host(Arc::new(Immediate(Box::new(|_| ok_response("0123456789")))));
    let id = host.start_fetch(FetchRequest::get(url("https://example.com/thing")));
    host.take_ready();

    host.with_response(id, |r| r.body.len()).unwrap();
    host.with_response(id, |r| r.body.len()).unwrap();

    assert_eq!(host.counters().fetch_bytes_received, 10);
}

#[test]
fn storage_counters_count_hits_and_not_misses() {
    let host = host_at("https://example.com/", Arc::new(MemoryStorage::new()));

    host.storage_set("k", "value").unwrap();
    host.storage_get("k");
    host.storage_get("absent");

    let counters = host.counters();
    assert_eq!(counters.storage_writes, 1);
    assert_eq!(counters.storage_bytes_written, 6, "key `k` plus `value`");
    assert_eq!(counters.storage_reads, 2);
    assert_eq!(
        counters.storage_bytes_read, 5,
        "the miss contributes nothing"
    );
}
