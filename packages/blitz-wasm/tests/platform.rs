//! The platform imports, driven by a real wasm guest under wasmi.
//!
//! # The guest is a pass-through, and that is the point
//!
//! Every export below just forwards its arguments to one import. That makes the
//! *host test* the thing choosing the pointers and capacities, so it can hand
//! the boundary a buffer one byte too small, a capacity of zero, and a pointer
//! past the end of linear memory. A guest compiled from Rust would have to go
//! out of its way to produce any of those, and they are precisely the cases the
//! read protocol exists to define.
//!
//! # There is no document anywhere in this file
//!
//! The store's data is [`PlatformOnly`], which holds a `PlatformHost` and a
//! `PlatformState` and nothing else. It compiles, links and runs, which is the
//! evidence that the platform imports are genuinely independent of the DOM
//! rather than merely documented as such. A binding that had quietly reached
//! for a `BaseDocument` would not build against this type.

use std::sync::{Arc, Mutex};

use blitz_platform_api::{MemoryStorage, PlatformHost};
use blitz_traits::platform::{
    Bytes, FetchError, FetchHandler, FetchProvider, FetchRequest, FetchResponse, OriginKey,
    StatusCode, StorageProvider, Url,
};
use blitz_wasm::{HasPlatform, PlatformState, add_platform_to_linker, dispatch_fetch_completions};
use dom_abi::host::{OutBuffer, ReadOutcome, Status};
use wasmi::{Engine, Instance, Linker, Module, Store, TypedFunc};

/// A store payload with a platform host and no document.
struct PlatformOnly {
    platform: Option<PlatformHost>,
    state: PlatformState,
}

impl HasPlatform for PlatformOnly {
    fn platform(&self) -> Option<&PlatformHost> {
        self.platform.as_ref()
    }
    fn platform_state(&mut self) -> &mut PlatformState {
        &mut self.state
    }
}

/// Every import, forwarded. Plus a `fetch_complete` that records what it was
/// called with, so a test can assert delivery rather than assuming it.
const GUEST: &str = r#"
(module
  (import "blitz" "fetch_new"         (func $fetch_new         (param i32 i32 i32 i32) (result i32)))
  (import "blitz" "fetch_header"      (func $fetch_header      (param i32 i32 i32 i32 i32) (result i32)))
  (import "blitz" "fetch_body"        (func $fetch_body        (param i32 i32 i32) (result i32)))
  (import "blitz" "fetch_send"        (func $fetch_send        (param i32) (result i32)))
  (import "blitz" "fetch_status"      (func $fetch_status      (param i32) (result i32)))
  (import "blitz" "fetch_read_body"   (func $fetch_read_body   (param i32 i32 i32) (result i32)))
  (import "blitz" "fetch_read_header" (func $fetch_read_header (param i32 i32 i32 i32 i32) (result i32)))
  (import "blitz" "fetch_read_url"    (func $fetch_read_url    (param i32 i32 i32) (result i32)))
  (import "blitz" "fetch_release"     (func $fetch_release     (param i32) (result i32)))
  (import "blitz" "storage_get"       (func $storage_get       (param i32 i32 i32 i32) (result i32)))
  (import "blitz" "storage_set"       (func $storage_set       (param i32 i32 i32 i32) (result i32)))
  (import "blitz" "storage_remove"    (func $storage_remove    (param i32 i32) (result i32)))
  (import "blitz" "storage_clear"     (func $storage_clear     (param) (result i32)))

  (memory (export "memory") 1)

  ;; How many times fetch_complete ran, and the last id it saw. Kept high in
  ;; the page so string scratch space at low addresses never collides.
  (global $completions (mut i32) (i32.const 0))
  (global $last_id (mut i32) (i32.const -1))
  (global $guest_status (mut i32) (i32.const 0))

  (func (export "fetch_complete") (param $id i32) (result i32)
    (global.set $completions (i32.add (global.get $completions) (i32.const 1)))
    (global.set $last_id (local.get $id))
    (global.get $guest_status))

  (func (export "completions") (result i32) (global.get $completions))
  (func (export "last_id") (result i32) (global.get $last_id))
  (func (export "set_guest_status") (param i32) (global.set $guest_status (local.get 0)))

  (func (export "call_fetch_new") (param i32 i32 i32 i32) (result i32)
    (call $fetch_new (local.get 0) (local.get 1) (local.get 2) (local.get 3)))
  (func (export "call_fetch_header") (param i32 i32 i32 i32 i32) (result i32)
    (call $fetch_header (local.get 0) (local.get 1) (local.get 2) (local.get 3) (local.get 4)))
  (func (export "call_fetch_body") (param i32 i32 i32) (result i32)
    (call $fetch_body (local.get 0) (local.get 1) (local.get 2)))
  (func (export "call_fetch_send") (param i32) (result i32)
    (call $fetch_send (local.get 0)))
  (func (export "call_fetch_status") (param i32) (result i32)
    (call $fetch_status (local.get 0)))
  (func (export "call_fetch_read_body") (param i32 i32 i32) (result i32)
    (call $fetch_read_body (local.get 0) (local.get 1) (local.get 2)))
  (func (export "call_fetch_read_header") (param i32 i32 i32 i32 i32) (result i32)
    (call $fetch_read_header (local.get 0) (local.get 1) (local.get 2) (local.get 3) (local.get 4)))
  (func (export "call_fetch_read_url") (param i32 i32 i32) (result i32)
    (call $fetch_read_url (local.get 0) (local.get 1) (local.get 2)))
  (func (export "call_fetch_release") (param i32) (result i32)
    (call $fetch_release (local.get 0)))
  (func (export "call_storage_get") (param i32 i32 i32 i32) (result i32)
    (call $storage_get (local.get 0) (local.get 1) (local.get 2) (local.get 3)))
  (func (export "call_storage_set") (param i32 i32 i32 i32) (result i32)
    (call $storage_set (local.get 0) (local.get 1) (local.get 2) (local.get 3)))
  (func (export "call_storage_remove") (param i32 i32) (result i32)
    (call $storage_remove (local.get 0) (local.get 1)))
  (func (export "call_storage_clear") (result i32)
    (call $storage_clear))
)
"#;

type Answer = Box<dyn Fn(&FetchRequest) -> Result<FetchResponse, FetchError> + Send + Sync>;

/// A provider that answers on the calling thread.
struct Immediate(Answer);

impl FetchProvider for Immediate {
    fn fetch(&self, request: FetchRequest, handler: Box<dyn FetchHandler>) {
        let result = (self.0)(&request);
        handler.complete(result);
    }
}

/// A provider that keeps the handler until a test releases it.
#[derive(Default)]
struct Deferred(Mutex<Vec<Box<dyn FetchHandler>>>);

impl Deferred {
    fn answer(&self, result: Result<FetchResponse, FetchError>) {
        for handler in self.0.lock().unwrap().drain(..) {
            handler.complete(match &result {
                Ok(response) => Ok(response.clone()),
                Err(error) => Err(error.clone()),
            });
        }
    }
}

impl FetchProvider for Deferred {
    fn fetch(&self, _request: FetchRequest, handler: Box<dyn FetchHandler>) {
        self.0.lock().unwrap().push(handler);
    }
}

fn ok_response(body: &str) -> FetchResponse {
    let mut headers = blitz_traits::platform::HeaderMap::new();
    headers.insert("content-type", "text/plain".parse().unwrap());
    headers.insert("x-empty", "".parse().unwrap());
    FetchResponse::new(
        Url::parse("https://example.com/final").unwrap(),
        StatusCode::OK,
    )
    .headers(headers)
    .body(Bytes::from(body.to_owned()))
}

/// The whole rig: a store with a platform host, and an instantiated guest.
struct Rig {
    store: Store<PlatformOnly>,
    instance: Instance,
}

fn rig_with(fetch: Arc<dyn FetchProvider>, storage: Arc<dyn StorageProvider>, origin: &str) -> Rig {
    rig_from(Some(PlatformHost::new(
        OriginKey::for_document(&Url::parse(origin).unwrap()),
        fetch,
        storage,
    )))
}

fn rig_from(platform: Option<PlatformHost>) -> Rig {
    let engine = Engine::default();
    let module = Module::new(
        &engine,
        wat::parse_str(GUEST).expect("the guest should parse"),
    )
    .expect("the guest should compile");
    let mut store = Store::new(
        &engine,
        PlatformOnly {
            platform,
            state: PlatformState::default(),
        },
    );
    let mut linker = Linker::new(&engine);
    add_platform_to_linker(&mut linker).expect("the imports should register");
    let instance = linker
        .instantiate_and_start(&mut store, &module)
        .expect("every import should be satisfied");

    Rig { store, instance }
}

fn default_rig() -> Rig {
    rig_with(
        Arc::new(Immediate(Box::new(|_| Ok(ok_response("hello"))))),
        Arc::new(MemoryStorage::new()),
        "https://example.com/",
    )
}

impl Rig {
    /// Put bytes into guest memory and return `(ptr, len)`.
    fn poke(&mut self, at: i32, text: &str) -> (i32, i32) {
        let memory = self
            .instance
            .get_memory(&self.store, "memory")
            .expect("the guest exports memory");
        let start = at as usize;
        memory.data_mut(&mut self.store)[start..start + text.len()]
            .copy_from_slice(text.as_bytes());
        (at, text.len() as i32)
    }

    fn peek(&self, at: i32, len: i32) -> Vec<u8> {
        let memory = self
            .instance
            .get_memory(&self.store, "memory")
            .expect("the guest exports memory");
        let start = at as usize;
        memory.data(&self.store)[start..start + len as usize].to_vec()
    }

    fn func<P: wasmi::WasmParams, R: wasmi::WasmResults>(&mut self, name: &str) -> TypedFunc<P, R> {
        self.instance
            .get_typed_func::<P, R>(&self.store, name)
            .unwrap_or_else(|err| panic!("the guest should export {name}: {err}"))
    }

    fn call1(&mut self, name: &str, a: i32) -> i32 {
        let f = self.func::<i32, i32>(name);
        f.call(&mut self.store, a).expect("no trap")
    }

    fn call2(&mut self, name: &str, a: i32, b: i32) -> i32 {
        let f = self.func::<(i32, i32), i32>(name);
        f.call(&mut self.store, (a, b)).expect("no trap")
    }

    fn call3(&mut self, name: &str, a: i32, b: i32, c: i32) -> i32 {
        let f = self.func::<(i32, i32, i32), i32>(name);
        f.call(&mut self.store, (a, b, c)).expect("no trap")
    }

    fn call4(&mut self, name: &str, a: i32, b: i32, c: i32, d: i32) -> i32 {
        let f = self.func::<(i32, i32, i32, i32), i32>(name);
        f.call(&mut self.store, (a, b, c, d)).expect("no trap")
    }

    fn call5(&mut self, name: &str, a: i32, b: i32, c: i32, d: i32, e: i32) -> i32 {
        let f = self.func::<(i32, i32, i32, i32, i32), i32>(name);
        f.call(&mut self.store, (a, b, c, d, e)).expect("no trap")
    }

    /// Build and send a GET, returning the guest's request id.
    fn send_get(&mut self, url: &str) -> i32 {
        let (mptr, mlen) = self.poke(0, "GET");
        let (uptr, ulen) = self.poke(16, url);
        let id = self.call4("call_fetch_new", mptr, mlen, uptr, ulen);
        assert!(id >= 0, "fetch_new failed with {id}");
        assert_eq!(self.call1("call_fetch_send", id), Status::OK.raw());
        id
    }

    fn drain(&mut self) -> blitz_wasm::Completed {
        dispatch_fetch_completions(&mut self.store, &self.instance).expect("no trap")
    }

    fn completions(&mut self) -> i32 {
        let f = self.func::<(), i32>("completions");
        f.call(&mut self.store, ()).unwrap()
    }

    fn last_id(&mut self) -> i32 {
        let f = self.func::<(), i32>("last_id");
        f.call(&mut self.store, ()).unwrap()
    }
}

// -- storage --------------------------------------------------------------

#[test]
fn storage_round_trips_through_the_boundary() {
    let mut rig = default_rig();

    let (kptr, klen) = rig.poke(0, "theme");
    let (vptr, vlen) = rig.poke(64, "dark");
    assert_eq!(
        rig.call4("call_storage_set", kptr, klen, vptr, vlen),
        Status::OK.raw()
    );

    let (kptr, klen) = rig.poke(0, "theme");
    let len = rig.call4("call_storage_get", kptr, klen, 128, 64);
    assert_eq!(len, 4);
    assert_eq!(rig.peek(128, len), b"dark");
}

#[test]
fn an_absent_key_is_absent_and_not_an_error() {
    let mut rig = default_rig();
    let (kptr, klen) = rig.poke(0, "never-set");

    let status = Status(rig.call4("call_storage_get", kptr, klen, 128, 64));
    assert_eq!(status, Status::ABSENT);
    assert!(!status.is_failure(), "ABSENT must not read as a failure");
    assert_eq!(
        ReadOutcome::classify(status, OutBuffer { ptr: 128, cap: 64 }),
        ReadOutcome::Absent
    );
}

/// The distinction `ABSENT` exists for. A key holding `""` is present.
#[test]
fn an_empty_value_is_present_and_is_not_absent() {
    let mut rig = default_rig();

    let (kptr, klen) = rig.poke(0, "empty");
    let (vptr, vlen) = rig.poke(64, "");
    rig.call4("call_storage_set", kptr, klen, vptr, vlen);

    let (kptr, klen) = rig.poke(0, "empty");
    let status = Status(rig.call4("call_storage_get", kptr, klen, 128, 64));
    assert_eq!(status, Status(0));
    assert_eq!(
        ReadOutcome::classify(status, OutBuffer { ptr: 128, cap: 64 }),
        ReadOutcome::Fit { len: 0 }
    );
}

#[test]
fn remove_and_clear_take_effect() {
    let mut rig = default_rig();

    for (key, value) in [("a", "1"), ("b", "2")] {
        let (kptr, klen) = rig.poke(0, key);
        let (vptr, vlen) = rig.poke(64, value);
        rig.call4("call_storage_set", kptr, klen, vptr, vlen);
    }

    let (kptr, klen) = rig.poke(0, "a");
    assert_eq!(
        rig.call2("call_storage_remove", kptr, klen),
        Status::OK.raw()
    );
    let (kptr, klen) = rig.poke(0, "a");
    assert_eq!(
        Status(rig.call4("call_storage_get", kptr, klen, 128, 64)),
        Status::ABSENT
    );

    let f = rig.func::<(), i32>("call_storage_clear");
    assert_eq!(f.call(&mut rig.store, ()).unwrap(), Status::OK.raw());
    let (kptr, klen) = rig.poke(0, "b");
    assert_eq!(
        Status(rig.call4("call_storage_get", kptr, klen, 128, 64)),
        Status::ABSENT
    );
}

/// One store, two guests, two origins. The guest never names an origin and
/// cannot: it is not in any signature.
#[test]
fn two_guests_at_different_origins_do_not_share_storage() {
    let store: Arc<dyn StorageProvider> = Arc::new(MemoryStorage::new());
    let provider: Arc<dyn FetchProvider> = Arc::new(Immediate(Box::new(|_| Ok(ok_response("x")))));

    let mut one = rig_with(provider.clone(), store.clone(), "https://one.example/");
    let mut two = rig_with(provider, store, "https://two.example/");

    let (kptr, klen) = one.poke(0, "token");
    let (vptr, vlen) = one.poke(64, "one-secret");
    one.call4("call_storage_set", kptr, klen, vptr, vlen);

    let (kptr, klen) = two.poke(0, "token");
    assert_eq!(
        Status(two.call4("call_storage_get", kptr, klen, 128, 64)),
        Status::ABSENT,
        "a second origin must not see the first's value"
    );
}

/// **The persistence property, at the boundary.** A guest writes, the whole
/// instance is destroyed, a new one is built on the same origin against the
/// same store, and the value is still there. The store outlives the instance,
/// which is what "survives a restart" means for everything above it.
#[test]
fn a_value_survives_the_instance_that_wrote_it() {
    let store: Arc<dyn StorageProvider> = Arc::new(MemoryStorage::new());
    let provider: Arc<dyn FetchProvider> = Arc::new(Immediate(Box::new(|_| Ok(ok_response("x")))));

    {
        let mut first = rig_with(provider.clone(), store.clone(), "https://example.com/");
        let (kptr, klen) = first.poke(0, "session");
        let (vptr, vlen) = first.poke(64, "abc123");
        first.call4("call_storage_set", kptr, klen, vptr, vlen);
    }

    let mut second = rig_with(provider, store, "https://example.com/");
    let (kptr, klen) = second.poke(0, "session");
    let len = second.call4("call_storage_get", kptr, klen, 128, 64);
    assert_eq!(len, 6);
    assert_eq!(second.peek(128, len), b"abc123");
}

// -- the read protocol ----------------------------------------------------

/// `snprintf`'s convention: the full length comes back, nothing is written.
#[test]
fn a_buffer_one_byte_too_small_writes_nothing_and_reports_the_need() {
    let mut rig = default_rig();

    let (kptr, klen) = rig.poke(0, "k");
    let (vptr, vlen) = rig.poke(64, "0123456789");
    rig.call4("call_storage_set", kptr, klen, vptr, vlen);

    // Poison the destination so "wrote nothing" is observable rather than
    // assumed.
    rig.poke(128, "##########");

    let (kptr, klen) = rig.poke(0, "k");
    let status = Status(rig.call4("call_storage_get", kptr, klen, 128, 9));
    assert_eq!(
        ReadOutcome::classify(status, OutBuffer { ptr: 128, cap: 9 }),
        ReadOutcome::TooSmall { needed: 10 }
    );
    assert_eq!(
        rig.peek(128, 10),
        b"##########",
        "nothing may be written when the value does not fit"
    );

    // The retry is sized from the host's own answer, so it cannot come back
    // short.
    let (kptr, klen) = rig.poke(0, "k");
    let len = rig.call4("call_storage_get", kptr, klen, 128, 10);
    assert_eq!(len, 10);
    assert_eq!(rig.peek(128, 10), b"0123456789");
}

#[test]
fn a_zero_capacity_read_asks_for_a_length() {
    let mut rig = default_rig();

    let (kptr, klen) = rig.poke(0, "k");
    let (vptr, vlen) = rig.poke(64, "twelve chars");
    rig.call4("call_storage_set", kptr, klen, vptr, vlen);

    let (kptr, klen) = rig.poke(0, "k");
    let status = Status(rig.call4("call_storage_get", kptr, klen, 0, 0));
    assert_eq!(
        ReadOutcome::classify(status, OutBuffer { ptr: 0, cap: 0 }),
        ReadOutcome::TooSmall { needed: 12 }
    );
}

/// A pointer past the end of linear memory is an error return, never a trap.
#[test]
fn a_pointer_outside_memory_is_an_error_and_the_instance_survives() {
    let mut rig = default_rig();

    let (kptr, klen) = rig.poke(0, "k");
    let (vptr, vlen) = rig.poke(64, "value");
    rig.call4("call_storage_set", kptr, klen, vptr, vlen);

    // One page is 65536 bytes.
    let (kptr, klen) = rig.poke(0, "k");
    let status = Status(rig.call4("call_storage_get", kptr, klen, 65_000, 4_096));
    assert_eq!(status, Status::ERR_BAD_MEMORY);

    // Still usable, which is the property that matters.
    let (kptr, klen) = rig.poke(0, "k");
    let len = rig.call4("call_storage_get", kptr, klen, 128, 64);
    assert_eq!(len, 5);
}

/// A `cap == 0` buffer is still a buffer, and its pointer is still checked.
///
/// `cap == 0` is how a guest asks for a length with nowhere to put the value,
/// so the capacity is legal — but if the bounds check is skipped for it, the
/// answer starts depending on what happens to be stored. A value too long to
/// fit returns early without ever looking at the pointer; an empty value falls
/// through to the write, where `get_mut` rejects it. Same buffer, same call,
/// two different answers.
#[test]
fn a_zero_capacity_buffer_with_a_wild_pointer_fails_whatever_the_value_length() {
    let mut rig = default_rig();

    // One page is 65536 bytes, so this names a region outside memory.
    const WILD: i32 = 70_000;

    let (kptr, klen) = rig.poke(0, "full");
    let (vptr, vlen) = rig.poke(64, "value");
    assert_eq!(
        rig.call4("call_storage_set", kptr, klen, vptr, vlen),
        Status::OK.raw()
    );

    let (kptr, klen) = rig.poke(0, "empty");
    let (vptr, vlen) = rig.poke(64, "");
    assert_eq!(
        rig.call4("call_storage_set", kptr, klen, vptr, vlen),
        Status::OK.raw()
    );

    let (kptr, klen) = rig.poke(0, "full");
    assert_eq!(
        Status(rig.call4("call_storage_get", kptr, klen, WILD, 0)),
        Status::ERR_BAD_MEMORY,
        "a value that does not fit must not skip the pointer check"
    );

    let (kptr, klen) = rig.poke(0, "empty");
    assert_eq!(
        Status(rig.call4("call_storage_get", kptr, klen, WILD, 0)),
        Status::ERR_BAD_MEMORY,
        "and an empty value must answer the same way"
    );

    // A legal `cap == 0` probe still works: it reports the length and writes
    // nothing. This is the case the unconditional check must not break.
    let (kptr, klen) = rig.poke(0, "full");
    assert_eq!(rig.call4("call_storage_get", kptr, klen, 128, 0), 5);
    let (kptr, klen) = rig.poke(0, "empty");
    assert_eq!(rig.call4("call_storage_get", kptr, klen, 128, 0), 0);

    // Including one whose pointer sits exactly at the end of memory, which
    // names an empty region and is in bounds.
    let (kptr, klen) = rig.poke(0, "empty");
    assert_eq!(rig.call4("call_storage_get", kptr, klen, 65_536, 0), 0);
}

// -- fetch ----------------------------------------------------------------

#[test]
fn a_guest_fetches_a_url_and_reads_the_body() {
    let mut rig = default_rig();
    let id = rig.send_get("https://example.com/thing");

    assert_eq!(rig.drain().delivered, 1);
    assert_eq!(rig.completions(), 1);
    assert_eq!(rig.last_id(), id, "the guest is told the id it was given");

    assert_eq!(rig.call1("call_fetch_status", id), 200);

    let len = rig.call3("call_fetch_read_body", id, 256, 64);
    assert_eq!(len, 5);
    assert_eq!(rig.peek(256, len), b"hello");
}

/// The reason the whole platform layer exists. The resource loader turns this
/// into an error it logs and drops; here it is a status the guest can read.
#[test]
fn a_404_reaches_the_guest_as_a_status_with_a_body() {
    let mut rig = rig_with(
        Arc::new(Immediate(Box::new(|_| {
            Ok(FetchResponse::new(
                Url::parse("https://example.com/x").unwrap(),
                StatusCode::NOT_FOUND,
            )
            .body(Bytes::from_static(b"missing")))
        }))),
        Arc::new(MemoryStorage::new()),
        "https://example.com/",
    );

    let id = rig.send_get("https://example.com/x");
    rig.drain();

    assert_eq!(rig.call1("call_fetch_status", id), 404);
    let len = rig.call3("call_fetch_read_body", id, 256, 64);
    assert_eq!(rig.peek(256, len), b"missing");
}

#[test]
fn a_failed_request_is_distinguishable_from_a_response() {
    let mut rig = rig_with(
        Arc::new(Immediate(Box::new(|_| {
            Err(FetchError::Network("dns".into()))
        }))),
        Arc::new(MemoryStorage::new()),
        "https://example.com/",
    );

    let id = rig.send_get("https://nowhere.invalid/");
    assert_eq!(rig.drain().delivered, 1);

    assert_eq!(
        Status(rig.call1("call_fetch_status", id)),
        Status::ERR_FETCH
    );
    assert_eq!(
        Status(rig.call3("call_fetch_read_body", id, 256, 64)),
        Status::ERR_FETCH
    );
}

#[test]
fn reading_before_completion_says_pending_rather_than_answering_empty() {
    let provider = Arc::new(Deferred::default());
    let mut rig = rig_with(
        provider.clone(),
        Arc::new(MemoryStorage::new()),
        "https://example.com/",
    );

    let id = rig.send_get("https://example.com/slow");
    assert_eq!(
        Status(rig.call1("call_fetch_status", id)),
        Status::ERR_REQUEST_PENDING
    );
    assert_eq!(
        Status(rig.call3("call_fetch_read_body", id, 256, 64)),
        Status::ERR_REQUEST_PENDING
    );
    assert_eq!(rig.drain().delivered, 0, "nothing has completed yet");

    provider.answer(Ok(ok_response("late")));
    assert_eq!(rig.drain().delivered, 1);
    assert_eq!(rig.call1("call_fetch_status", id), 200);
}

#[test]
fn a_response_header_reads_back_and_a_missing_one_is_absent() {
    let mut rig = default_rig();
    let id = rig.send_get("https://example.com/thing");
    rig.drain();

    let (nptr, nlen) = rig.poke(0, "content-type");
    let len = rig.call5("call_fetch_read_header", id, nptr, nlen, 256, 64);
    assert_eq!(rig.peek(256, len), b"text/plain");

    let (nptr, nlen) = rig.poke(0, "x-not-sent");
    let status = Status(rig.call5("call_fetch_read_header", id, nptr, nlen, 256, 64));
    assert_eq!(status, Status::ABSENT);

    // Present but empty is not absent, the same distinction storage makes.
    let (nptr, nlen) = rig.poke(0, "x-empty");
    let status = Status(rig.call5("call_fetch_read_header", id, nptr, nlen, 256, 64));
    assert_eq!(status, Status(0));
}

#[test]
fn the_final_url_is_readable() {
    let mut rig = default_rig();
    let id = rig.send_get("https://example.com/thing");
    rig.drain();

    let len = rig.call3("call_fetch_read_url", id, 256, 128);
    assert_eq!(rig.peek(256, len), b"https://example.com/final");
}

#[test]
fn a_forged_request_id_is_an_error_not_a_trap() {
    let mut rig = default_rig();

    assert_eq!(
        Status(rig.call1("call_fetch_status", 9_999)),
        Status::ERR_BAD_REQUEST
    );
    assert_eq!(
        Status(rig.call1("call_fetch_send", 9_999)),
        Status::ERR_BAD_REQUEST
    );
    assert_eq!(
        Status(rig.call1("call_fetch_release", 9_999)),
        Status::ERR_BAD_REQUEST
    );
    assert_eq!(
        Status(rig.call1("call_fetch_status", -1)),
        Status::ERR_BAD_REQUEST
    );

    // The instance still works.
    let id = rig.send_get("https://example.com/thing");
    rig.drain();
    assert_eq!(rig.call1("call_fetch_status", id), 200);
}

#[test]
fn a_released_request_is_not_delivered_and_ids_are_not_reused() {
    let provider = Arc::new(Deferred::default());
    let mut rig = rig_with(
        provider.clone(),
        Arc::new(MemoryStorage::new()),
        "https://example.com/",
    );

    let first = rig.send_get("https://example.com/a");
    assert_eq!(rig.call1("call_fetch_release", first), Status::OK.raw());

    provider.answer(Ok(ok_response("too late")));
    let drained = rig.drain();
    assert_eq!(drained.delivered, 0, "a released request is not announced");
    assert_eq!(rig.completions(), 0);

    let second = rig.send_get("https://example.com/b");
    assert_ne!(first, second, "ids are never reused");
}

#[test]
fn sending_twice_is_refused_and_a_draft_cannot_be_read() {
    let mut rig = default_rig();
    let (mptr, mlen) = rig.poke(0, "GET");
    let (uptr, ulen) = rig.poke(16, "https://example.com/thing");
    let id = rig.call4("call_fetch_new", mptr, mlen, uptr, ulen);

    assert_eq!(
        Status(rig.call1("call_fetch_status", id)),
        Status::ERR_REQUEST_PENDING,
        "a draft has no status yet"
    );

    assert_eq!(rig.call1("call_fetch_send", id), Status::OK.raw());
    assert_eq!(
        Status(rig.call1("call_fetch_send", id)),
        Status::ERR_ALREADY_SENT
    );
}

#[test]
fn a_bad_url_and_a_bad_method_are_reported_separately() {
    let mut rig = default_rig();

    let (mptr, mlen) = rig.poke(0, "GET");
    let (uptr, ulen) = rig.poke(16, "not a url at all");
    assert_eq!(
        Status(rig.call4("call_fetch_new", mptr, mlen, uptr, ulen)),
        Status::ERR_BAD_URL
    );

    let (mptr, mlen) = rig.poke(0, "BAD METHOD");
    let (uptr, ulen) = rig.poke(16, "https://example.com/");
    assert_eq!(
        Status(rig.call4("call_fetch_new", mptr, mlen, uptr, ulen)),
        Status::ERR_BAD_HEADER
    );
}

#[test]
fn a_request_carries_its_headers_and_body_to_the_provider() {
    type Seen = Arc<Mutex<Option<(String, Vec<u8>, Option<String>)>>>;
    let seen: Seen = Arc::default();
    let recorder = seen.clone();

    let mut rig = rig_with(
        Arc::new(Immediate(Box::new(move |request| {
            *recorder.lock().unwrap() = Some((
                request.method.to_string(),
                request.body.clone().map(|b| b.to_vec()).unwrap_or_default(),
                request
                    .headers
                    .get("x-token")
                    .map(|v| v.to_str().unwrap().to_owned()),
            ));
            Ok(ok_response("ok"))
        }))),
        Arc::new(MemoryStorage::new()),
        "https://example.com/",
    );

    let (mptr, mlen) = rig.poke(0, "POST");
    let (uptr, ulen) = rig.poke(16, "https://example.com/submit");
    let id = rig.call4("call_fetch_new", mptr, mlen, uptr, ulen);

    let (nptr, nlen) = rig.poke(64, "x-token");
    let (vptr, vlen) = rig.poke(96, "sekrit");
    assert_eq!(
        rig.call5("call_fetch_header", id, nptr, nlen, vptr, vlen),
        Status::OK.raw()
    );

    let (bptr, blen) = rig.poke(128, "{\"a\":1}");
    assert_eq!(
        rig.call3("call_fetch_body", id, bptr, blen),
        Status::OK.raw()
    );
    rig.call1("call_fetch_send", id);

    let (method, body, token) = seen.lock().unwrap().clone().expect("the provider ran");
    assert_eq!(method, "POST");
    assert_eq!(body, b"{\"a\":1}");
    assert_eq!(token.as_deref(), Some("sekrit"));
}

#[test]
fn a_guest_that_reports_a_failure_is_counted_but_does_not_stop_the_drain() {
    let mut rig = default_rig();
    let setter = rig.func::<i32, ()>("set_guest_status");
    setter.call(&mut rig.store, -7).unwrap();

    rig.send_get("https://example.com/a");
    rig.send_get("https://example.com/b");

    let drained = rig.drain();
    assert_eq!(drained.drained, 2);
    assert_eq!(
        drained.delivered, 2,
        "the second is delivered after the first fails"
    );
    assert_eq!(drained.failed, 2);
}

/// An embedder may bind the imports and install no providers. That must answer,
/// not panic: the failure belongs to the embedding and the guest cannot fix it.
#[test]
fn without_a_platform_host_every_import_answers_no_platform() {
    let mut rig = rig_from(None);

    let (kptr, klen) = rig.poke(0, "k");
    assert_eq!(
        Status(rig.call4("call_storage_get", kptr, klen, 128, 64)),
        Status::ERR_NO_PLATFORM
    );

    let (mptr, mlen) = rig.poke(0, "GET");
    let (uptr, ulen) = rig.poke(16, "https://example.com/");
    let id = rig.call4("call_fetch_new", mptr, mlen, uptr, ulen);
    assert!(id >= 0, "building a request needs no provider");
    assert_eq!(
        Status(rig.call1("call_fetch_send", id)),
        Status::ERR_NO_PLATFORM
    );

    assert_eq!(rig.drain(), blitz_wasm::Completed::default());
}

/// `ERR_NO_PLATFORM` from `fetch_send` must leave the draft where it was.
///
/// The status means "this embedding has no platform host", which is not the
/// guest's fault and invites a retry once one is installed. Answering it while
/// destroying the method, URL, headers and body would make that retry
/// impossible, and would turn every later call on the id into
/// `ERR_BAD_REQUEST` — a code that says the guest passed a bad id when in fact
/// the host threw its request away.
#[test]
fn a_send_that_finds_no_platform_host_leaves_the_request_usable() {
    let mut rig = rig_from(None);

    let (mptr, mlen) = rig.poke(0, "POST");
    let (uptr, ulen) = rig.poke(16, "https://example.com/submit");
    let id = rig.call4("call_fetch_new", mptr, mlen, uptr, ulen);
    assert!(id >= 0);

    let (nptr, nlen) = rig.poke(64, "x-token");
    let (vptr, vlen) = rig.poke(96, "sekrit");
    assert_eq!(
        rig.call5("call_fetch_header", id, nptr, nlen, vptr, vlen),
        Status::OK.raw()
    );
    let (bptr, blen) = rig.poke(128, "{\"a\":1}");
    assert_eq!(
        rig.call3("call_fetch_body", id, bptr, blen),
        Status::OK.raw()
    );

    assert_eq!(
        Status(rig.call1("call_fetch_send", id)),
        Status::ERR_NO_PLATFORM
    );

    // The id still names a draft. `ERR_BAD_REQUEST` here would mean the entry
    // was taken out for the send and never put back.
    let (nptr, nlen) = rig.poke(64, "x-second");
    let (vptr, vlen) = rig.poke(96, "also-sekrit");
    assert_eq!(
        Status(rig.call5("call_fetch_header", id, nptr, nlen, vptr, vlen)),
        Status::OK,
        "the draft survived a send that found no platform host"
    );

    assert_eq!(
        rig.store.data().state.live_requests(),
        1,
        "and it is still counted as live"
    );
}

// -- counters -------------------------------------------------------------

#[test]
fn fetch_bytes_and_storage_bytes_are_counted_separately_in_both_directions() {
    let mut rig = default_rig();

    let (kptr, klen) = rig.poke(0, "k");
    let (vptr, vlen) = rig.poke(64, "value");
    rig.call4("call_storage_set", kptr, klen, vptr, vlen);
    let (kptr, klen) = rig.poke(0, "k");
    rig.call4("call_storage_get", kptr, klen, 128, 64);

    let id = rig.send_get("https://example.com/thing");
    rig.drain();
    rig.call3("call_fetch_read_body", id, 256, 64);

    let counters = *rig.store.data().state.counters();

    // Storage: in is "k" + "value" for the set plus "k" for the get; out is
    // the five bytes of the value coming back.
    assert_eq!(counters.storage_set.bytes_in, 6);
    assert_eq!(counters.storage_get.bytes_in, 1);
    assert_eq!(counters.storage_get.bytes_out, 5);
    assert_eq!(counters.storage_bytes(), 12);

    // Fetch: in is "GET" plus the URL; out is the body.
    assert_eq!(counters.fetch_new.bytes_in, 3 + 25);
    assert_eq!(counters.fetch_read_body.bytes_out, 5);
    assert_eq!(counters.fetch_bytes(), 33);

    // The two never merge, which is the whole point of the split.
    assert_eq!(
        counters.total_bytes_in() + counters.total_bytes_out(),
        counters.fetch_bytes() + counters.storage_bytes()
    );
    assert_eq!(counters.fetch_complete.bytes_in, 0);
    assert_eq!(counters.fetch_complete.bytes_out, 0);
}

/// A read that did not fit wrote nothing, and the counter must say so.
#[test]
fn a_read_that_did_not_fit_counts_no_bytes_out() {
    let mut rig = default_rig();

    let (kptr, klen) = rig.poke(0, "k");
    let (vptr, vlen) = rig.poke(64, "0123456789");
    rig.call4("call_storage_set", kptr, klen, vptr, vlen);

    let (kptr, klen) = rig.poke(0, "k");
    rig.call4("call_storage_get", kptr, klen, 128, 4);
    assert_eq!(rig.store.data().state.counters().storage_get.bytes_out, 0);

    let (kptr, klen) = rig.poke(0, "k");
    rig.call4("call_storage_get", kptr, klen, 128, 16);
    assert_eq!(rig.store.data().state.counters().storage_get.bytes_out, 10);
}

#[test]
fn absent_does_not_poison_the_last_error_slot() {
    let mut rig = default_rig();

    let (kptr, klen) = rig.poke(0, "never-set");
    rig.call4("call_storage_get", kptr, klen, 128, 64);
    assert_eq!(
        rig.store.data().state.counters().last_error,
        None,
        "polling an optional key must not look like a failure"
    );

    assert_eq!(
        Status(rig.call1("call_fetch_status", 9_999)),
        Status::ERR_BAD_REQUEST
    );
    assert_eq!(
        rig.store.data().state.counters().last_error,
        Some(Status::ERR_BAD_REQUEST)
    );
}
