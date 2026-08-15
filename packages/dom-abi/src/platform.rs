//! The platform contracts: `fetch` and storage.
//!
//! [`host`](crate::host) is the DOM calling convention. This is the same
//! convention applied to the platform APIs a guest expects a browser to have,
//! and it is a separate module for one reason: the two surfaces version
//! independently. Adding a `fetch` import must not bump
//! [`HOST_ABI_VERSION`](crate::host::HOST_ABI_VERSION) and invalidate a guest
//! that only touches the DOM.
//!
//! Everything else is shared and deliberately not restated here.
//! [`Status`](crate::host::Status) is the same type with the same rules,
//! [`OutBuffer`](crate::host::OutBuffer) is the same read protocol, and
//! [`MAX_ID`](crate::host::MAX_ID) is the same cap. A reader for a response
//! body and a reader for an attribute behave identically, which is the whole
//! point of putting them in one crate.

use serde::{Deserialize, Serialize};

use crate::host::Status;

/// The version of the platform calling convention.
///
/// Independent of [`HOST_ABI_VERSION`](crate::host::HOST_ABI_VERSION). A guest
/// that uses only the DOM imports is unaffected by anything here.
pub const PLATFORM_ABI_VERSION: u32 = 1;

/// An in-flight or completed `fetch`, as the guest sees it.
///
/// # Why the binding issues this and not the platform layer
///
/// A guest builds a request before it is sent, and the layer underneath only
/// learns of the request at send time. Passing that layer's id straight through
/// would mean the id a guest holds **changes halfway through the request's
/// life**, which is not something a guest can hold onto. So the binding issues
/// one id at `fetch_new` and it stays valid until `fetch_release`.
///
/// Never reused, and capped at [`MAX_ID`](crate::host::MAX_ID), for the same
/// reasons as [`Handle`](crate::host::Handle) and
/// [`ListenerId`](crate::host::ListenerId): a stale id must be an error rather
/// than a silent hit on whatever took its place.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct RequestId(pub u32);

impl Status {
    // The platform block starts at -20.
    //
    // `host` owns -1 to -9. The gap from -10 to -19 is reserved so the DOM
    // range can grow contiguously without ever colliding with this one, and so
    // that a code read from a log tells you which surface produced it.

    /// The request id was never issued by this instance, or has been released.
    pub const ERR_BAD_REQUEST: Status = Status(-20);

    /// The request has not completed yet.
    ///
    /// A guest must wait to be told. Distinct from a *failure*: the request is
    /// fine, the answer is simply not here.
    pub const ERR_REQUEST_PENDING: Status = Status(-21);

    /// The request finished without a response at all: DNS, TLS, connection
    /// refused, no provider installed.
    ///
    /// **Not an HTTP error status.** A server that answered 404 produced a
    /// *response*, and a reader returns 404. This is for when nothing answered.
    /// Collapsing the two is the defect the platform layer exists to prevent:
    /// the resource-loading path does exactly that and a guest can never see a
    /// 404 through it.
    pub const ERR_FETCH: Status = Status(-22);

    /// More than [`MAX_ID`](crate::host::MAX_ID) live requests.
    pub const ERR_TOO_MANY_REQUESTS: Status = Status(-23);

    /// The URL did not parse.
    pub const ERR_BAD_URL: Status = Status(-24);

    /// The method, a header name, or a header value was not valid HTTP.
    pub const ERR_BAD_HEADER: Status = Status(-25);

    /// The storage write was refused: quota, or the backing store failed.
    ///
    /// Only writes fail. An absent key is [`Status::ABSENT`], and removing what
    /// is not there is a no-op, which is what the storage API specifies.
    pub const ERR_STORAGE: Status = Status(-26);

    /// This instance has no platform host, so there is nothing to ask.
    ///
    /// An embedder that binds the platform imports and installs no providers
    /// has made a mistake belonging entirely to the embedding. The guest gets a
    /// status rather than a trap, because killing it tells nobody anything.
    pub const ERR_NO_PLATFORM: Status = Status(-27);

    /// `fetch_send` was called twice on one request.
    pub const ERR_ALREADY_SENT: Status = Status(-28);

    /// A human-readable name for a platform status, or `None` if it is not one.
    ///
    /// Separate from [`Status::name`](crate::host::Status::name) so that
    /// neither module has to know the other's codes. [`status_name`] tries both.
    pub fn platform_name(self) -> Option<&'static str> {
        Some(match self {
            Status::ERR_BAD_REQUEST => "ERR_BAD_REQUEST",
            Status::ERR_REQUEST_PENDING => "ERR_REQUEST_PENDING",
            Status::ERR_FETCH => "ERR_FETCH",
            Status::ERR_TOO_MANY_REQUESTS => "ERR_TOO_MANY_REQUESTS",
            Status::ERR_BAD_URL => "ERR_BAD_URL",
            Status::ERR_BAD_HEADER => "ERR_BAD_HEADER",
            Status::ERR_STORAGE => "ERR_STORAGE",
            Status::ERR_NO_PLATFORM => "ERR_NO_PLATFORM",
            Status::ERR_ALREADY_SENT => "ERR_ALREADY_SENT",
            _ => return None,
        })
    }
}

/// The name of any status, DOM or platform.
///
/// The one function a diagnostic should call, so that a test failure never has
/// to say "the guest got -22".
pub fn status_name(status: Status) -> &'static str {
    status.platform_name().unwrap_or_else(|| status.name())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The property the reserved gap exists to guarantee. If this fails, a
    /// guest cannot tell which surface rejected it.
    #[test]
    fn no_platform_code_collides_with_a_dom_code() {
        let dom = [
            Status::OK,
            Status::ERR_BAD_HANDLE,
            Status::ERR_BAD_ATOM,
            Status::ERR_BAD_MEMORY,
            Status::ERR_BAD_UTF8,
            Status::ERR_DOM,
            Status::ERR_TOO_MANY_HANDLES,
            Status::ERR_BAD_LISTENER,
            Status::ERR_TOO_MANY_LISTENERS,
            Status::ABSENT,
        ];
        let platform = [
            Status::ERR_BAD_REQUEST,
            Status::ERR_REQUEST_PENDING,
            Status::ERR_FETCH,
            Status::ERR_TOO_MANY_REQUESTS,
            Status::ERR_BAD_URL,
            Status::ERR_BAD_HEADER,
            Status::ERR_STORAGE,
            Status::ERR_NO_PLATFORM,
            Status::ERR_ALREADY_SENT,
        ];

        for one in platform {
            assert!(
                !dom.contains(&one),
                "{} collides with a DOM code",
                status_name(one)
            );
            assert!(
                one.raw() <= -20,
                "{} must be in the -20 block",
                status_name(one)
            );
        }

        let mut all: Vec<i32> = dom.iter().chain(&platform).map(|s| s.raw()).collect();
        let count = all.len();
        all.sort_unstable();
        all.dedup();
        assert_eq!(all.len(), count, "two status codes share a value");
    }

    /// Every platform code is a failure. None of them is a second `ABSENT`.
    #[test]
    fn every_platform_code_is_a_failure() {
        for one in [
            Status::ERR_BAD_REQUEST,
            Status::ERR_REQUEST_PENDING,
            Status::ERR_FETCH,
            Status::ERR_TOO_MANY_REQUESTS,
            Status::ERR_BAD_URL,
            Status::ERR_BAD_HEADER,
            Status::ERR_STORAGE,
            Status::ERR_NO_PLATFORM,
            Status::ERR_ALREADY_SENT,
        ] {
            assert!(one.is_failure(), "{} should be a failure", status_name(one));
            assert_eq!(one.value(), None);
        }
    }

    #[test]
    fn status_name_answers_for_both_surfaces() {
        assert_eq!(status_name(Status::ERR_BAD_HANDLE), "ERR_BAD_HANDLE");
        assert_eq!(status_name(Status::ERR_FETCH), "ERR_FETCH");
        assert_eq!(status_name(Status::ABSENT), "ABSENT (not an error)");
        assert_eq!(Status::ERR_BAD_HANDLE.platform_name(), None);
    }
}
