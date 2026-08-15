//! `blitz-net`'s [`FetchProvider`] implementation, end to end.
//!
//! # Why there is no HTTP here
//!
//! Every case below uses a `data:` or `file:` URL, and they go through the same
//! [`FetchProvider::fetch`] entry point, the same `spawn`, and the same
//! completion handler as an `https:` request does. What they do not need is a
//! network, a server, or a new dependency to stand one up.
//!
//! A test that reached a real host would be a test that fails on a train, and
//! one that started a local server would mean adding a server crate to prove
//! something about a client. The HTTP branch is exercised by the browser; what
//! is asserted here is the shape of the contract, which is where the bugs of
//! the "a 404 vanished" kind actually live.

use std::sync::{Arc, Mutex};

use blitz_net::Provider;
use blitz_traits::platform::{
    FetchError, FetchHandler, FetchProvider, FetchRequest, FetchResponse, StatusCode, Url,
};

/// Collects the one answer a request produces.
#[derive(Default)]
struct Sink(Arc<Mutex<Option<Result<FetchResponse, FetchError>>>>);

impl FetchHandler for Sink {
    fn complete(self: Box<Self>, result: Result<FetchResponse, FetchError>) {
        *self.0.lock().unwrap() = Some(result);
    }
}

/// Issue one request and wait for its answer.
///
/// Polls rather than using a channel because the provider may answer *before*
/// `fetch` returns: a `data:` URL is decoded inline. A oneshot receiver would
/// work too, but polling makes the "may already be done" case impossible to get
/// wrong.
async fn fetch_one(url: &str) -> Result<FetchResponse, FetchError> {
    let provider = Provider::new(None);
    let slot: Arc<Mutex<Option<Result<FetchResponse, FetchError>>>> = Arc::default();

    provider.fetch(
        FetchRequest::get(Url::parse(url).expect("test URL should parse")),
        Box::new(Sink(slot.clone())),
    );

    for _ in 0..2_000 {
        if let Some(answer) = slot.lock().unwrap().take() {
            return answer;
        }
        tokio::time::sleep(std::time::Duration::from_millis(5)).await;
    }
    panic!("the provider never answered for {url}");
}

#[tokio::test]
async fn a_data_url_answers_with_a_body_and_a_content_type() {
    let response = fetch_one("data:text/plain,hello%20world")
        .await
        .expect("a data URL should succeed");

    assert_eq!(response.status, StatusCode::OK);
    assert_eq!(response.body.as_ref(), b"hello world");
    assert_eq!(
        response
            .headers
            .get(reqwest::header::CONTENT_TYPE)
            .and_then(|value| value.to_str().ok()),
        Some("text/plain"),
        "the content type encoded in the URL is the response's"
    );
}

#[tokio::test]
async fn a_base64_data_url_is_decoded() {
    let response = fetch_one("data:application/json;base64,eyJvayI6dHJ1ZX0=")
        .await
        .expect("a base64 data URL should succeed");

    assert_eq!(response.body.as_ref(), br#"{"ok":true}"#);
}

#[tokio::test]
async fn a_file_url_answers_with_the_file() {
    let path = std::env::temp_dir().join("blitz-net-fetch-plain.txt");
    std::fs::write(&path, b"on disk").unwrap();

    let url = Url::from_file_path(&path).expect("a temp path should convert to a URL");
    let response = fetch_one(url.as_str())
        .await
        .expect("an existing file should succeed");

    assert_eq!(response.status, StatusCode::OK);
    assert_eq!(response.body.as_ref(), b"on disk");

    std::fs::remove_file(&path).ok();
}

/// The reason this path uses `Url::to_file_path` rather than `Url::path`, which
/// is what `NetProvider`'s own `file:` branch still uses.
///
/// `Url::path` hands back the percent-encoded path component, so this file is
/// looked up as `blitz-net%20fetch%20spaced.txt` and is not found. The failure
/// is a plain "no such file", which is exactly the kind that gets blamed on the
/// caller.
#[tokio::test]
async fn a_file_url_with_a_space_in_the_name_is_decoded_before_opening() {
    let path = std::env::temp_dir().join("blitz-net fetch spaced.txt");
    std::fs::write(&path, b"spaces are fine").unwrap();

    let url = Url::from_file_path(&path).expect("a temp path should convert to a URL");
    assert!(
        url.as_str().contains("%20"),
        "the URL should be percent-encoded, or this test proves nothing: {url}"
    );

    let response = fetch_one(url.as_str())
        .await
        .expect("a file with a space should be found");
    assert_eq!(response.body.as_ref(), b"spaces are fine");

    std::fs::remove_file(&path).ok();
}

#[tokio::test]
async fn a_missing_file_is_a_failure_and_not_an_empty_response() {
    let path = std::env::temp_dir().join("blitz-net-fetch-does-not-exist.txt");
    std::fs::remove_file(&path).ok();
    let url = Url::from_file_path(&path).unwrap();

    let error = fetch_one(url.as_str())
        .await
        .expect_err("a missing file has no response");
    assert!(
        matches!(error, FetchError::Network(_)),
        "expected a network-class failure, got {error:?}"
    );
}

#[tokio::test]
async fn an_unservable_scheme_says_so_rather_than_hanging() {
    let error = fetch_one("mailto:someone@example.com")
        .await
        .expect_err("mailto is not fetchable");

    match error {
        FetchError::UnsupportedScheme(scheme) => assert_eq!(scheme, "mailto"),
        other => panic!("expected UnsupportedScheme, got {other:?}"),
    }
}

/// One provider, many requests, and every handler gets its own answer. This is
/// the property that makes sharing the client safe.
#[tokio::test]
async fn concurrent_requests_do_not_cross_answers() {
    let provider = Arc::new(Provider::new(None));
    let slots: Vec<Arc<Mutex<Option<Result<FetchResponse, FetchError>>>>> =
        (0..8).map(|_| Arc::default()).collect();

    for (nth, slot) in slots.iter().enumerate() {
        provider.fetch(
            FetchRequest::get(Url::parse(&format!("data:text/plain,body-{nth}")).unwrap()),
            Box::new(Sink(slot.clone())),
        );
    }

    for _ in 0..2_000 {
        if slots.iter().all(|slot| slot.lock().unwrap().is_some()) {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(5)).await;
    }

    for (nth, slot) in slots.iter().enumerate() {
        let answer = slot.lock().unwrap().take().expect("every request answers");
        let response = answer.expect("every data URL succeeds");
        assert_eq!(
            response.body.as_ref(),
            format!("body-{nth}").as_bytes(),
            "request {nth} received another request's body"
        );
    }
}
