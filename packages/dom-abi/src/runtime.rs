//! How a module declares what it links against, and how that is checked.
//!
//! A guest module carries two custom sections. [`RUNTIME_SECTION`] says which
//! runtime it was compiled against and what it needs from it;
//! [`TEMPLATES_SECTION`] carries the templates it renders. Its imports are
//! namespaced by major version, [`IMPORT_NAMESPACE_PREFIX`].
//!
//! # The check happens before memory is shared
//!
//! **This is the ordering the module is shaped around.** Linking shares linear
//! memory. After that point a version mismatch is not a missing symbol — it is
//! two allocators disagreeing about one heap, which surfaces as corruption
//! somewhere unrelated, at a time unrelated, and with the host and the guest
//! each holding a plausible account of what happened.
//!
//! Before linking, the same mismatch is a struct comparison and a returned
//! error. So the types are shaped to make skipping the check awkward:
//! [`Linkage`] is the only thing this module hands out that a linking API can
//! ask for, and the only way to obtain one is [`RuntimeSection::link`]. A host
//! function that takes `&Linkage` cannot be called by a caller that has not
//! run the check, and there is no `Linkage::new`.
//!
//! That is a speed bump rather than a proof — a determined caller can run the
//! check, discard the result and link anyway. The value is that skipping it
//! stops being the path of least resistance, and starts being a line somebody
//! has to write on purpose.

use serde::{Deserialize, Serialize};

use crate::template::Template;

/// The version of the section shapes and the namespace convention.
///
/// **Independent of [`TEMPLATE_FORMAT_VERSION`] and [`HOST_ABI_VERSION`].**
/// This covers how a module *declares* what it links against. The declaration
/// format can change without the calling convention changing, and — more often
/// — the calling convention changes while the declaration stays exactly as it
/// was, which is the case that would otherwise force every module to be
/// rebuilt to say the same thing.
///
/// [`TEMPLATE_FORMAT_VERSION`]: crate::template::TEMPLATE_FORMAT_VERSION
/// [`HOST_ABI_VERSION`]: crate::host::HOST_ABI_VERSION
pub const RUNTIME_ABI_VERSION: u32 = 1;

/// The custom section naming the runtime a module was compiled against.
pub const RUNTIME_SECTION: &str = "pathscale.runtime";

/// The custom section carrying a module's templates.
pub const TEMPLATES_SECTION: &str = "pathscale.templates";

/// The prefix every import namespace carries.
///
/// The full namespace is this plus the major version — `solidrs:1`. Encoding
/// the major version in the namespace means an incompatible guest fails at
/// **instantiation**, with a missing-import error naming `solidrs:2`, rather
/// than linking successfully against a host that offers different semantics
/// under the same names. That is a second line of defence behind
/// [`RuntimeSection::link`], and unlike the first it cannot be skipped, because
/// the wasm runtime enforces it whether anybody remembered to or not.
pub const IMPORT_NAMESPACE_PREFIX: &str = "solidrs:";

/// The import namespace for a given major version.
pub fn import_namespace(major: u32) -> String {
    format!("{IMPORT_NAMESPACE_PREFIX}{major}")
}

/// The major version an import namespace names, if it is one of ours.
///
/// `None` for anything else, including a namespace with our prefix and a
/// non-numeric tail — `solidrs:next` is not a version, and treating it as one
/// would mean guessing.
pub fn parse_import_namespace(namespace: &str) -> Option<u32> {
    namespace
        .strip_prefix(IMPORT_NAMESPACE_PREFIX)?
        .parse()
        .ok()
}

/// The `pathscale.runtime` section: what the guest was built against.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RuntimeSection {
    /// Which runtime — `solidrs`. Compared for equality, not parsed.
    pub name: String,

    /// The major version. Must match the host's exactly: a major version is
    /// the statement that it does not.
    pub major: u32,

    /// The oldest minor version this module works against.
    ///
    /// Not "the minor it was built with". A module built against 1.4 that uses
    /// nothing added after 1.2 should link against a 1.2 host, and it is the
    /// module that knows which of those two numbers is true. Recording the
    /// build-time minor instead would reject working combinations, and the
    /// pressure that creates — hosts pinning minors upward to stay linkable —
    /// is how a minor version stops meaning anything.
    pub min_minor: u32,
}

impl RuntimeSection {
    /// Check this module against a host, before sharing memory.
    ///
    /// The only way to obtain a [`Linkage`].
    pub fn link(&self, host: &HostRuntime) -> Result<Linkage, Mismatch> {
        if self.name != host.name {
            return Err(Mismatch::DifferentRuntime {
                guest: self.name.clone(),
                host: host.name.clone(),
            });
        }
        if self.major != host.major {
            return Err(Mismatch::MajorMismatch {
                guest: self.major,
                host: host.major,
            });
        }
        if host.minor < self.min_minor {
            return Err(Mismatch::HostTooOld {
                guest_min_minor: self.min_minor,
                host_minor: host.minor,
            });
        }

        Ok(Linkage {
            namespace: import_namespace(self.major),
            major: self.major,
            minor: host.minor,
        })
    }
}

/// What a host offers.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HostRuntime {
    /// Which runtime this host implements — `solidrs`.
    pub name: String,
    /// The major version it implements.
    pub major: u32,
    /// The minor version it implements. Additive: a higher minor offers
    /// everything a lower one did.
    pub minor: u32,
}

/// Proof that the version check ran, and what it agreed on.
///
/// **There is no public constructor.** The only way to get one is
/// [`RuntimeSection::link`]. A linking API that takes `&Linkage` therefore
/// cannot be reached without running the check first, which is the whole point;
/// see this module's documentation for why the ordering matters and for the
/// limits of enforcing it this way.
///
/// Deliberately not serializable. It is a fact about a pairing that was checked
/// in this process, at this moment. Writing it down would create the thing it
/// exists to prevent: a check whose result outlives the conditions it was taken
/// under.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Linkage {
    namespace: String,
    major: u32,
    minor: u32,
}

impl Linkage {
    /// The import namespace both sides agreed on — `solidrs:1`.
    pub fn import_namespace(&self) -> &str {
        &self.namespace
    }

    /// The agreed major version.
    pub fn major(&self) -> u32 {
        self.major
    }

    /// The host's minor version, which is at least the guest's `min_minor`.
    pub fn minor(&self) -> u32 {
        self.minor
    }
}

/// Why a module and a host cannot be linked.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Mismatch {
    /// Different runtimes entirely.
    DifferentRuntime {
        /// What the module was built against.
        guest: String,
        /// What this host implements.
        host: String,
    },
    /// Incompatible major versions.
    MajorMismatch {
        /// The module's major.
        guest: u32,
        /// The host's major.
        host: u32,
    },
    /// The module needs a newer minor than this host implements.
    HostTooOld {
        /// The oldest minor the module works against.
        guest_min_minor: u32,
        /// What this host implements.
        host_minor: u32,
    },
}

impl std::fmt::Display for Mismatch {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Mismatch::DifferentRuntime { guest, host } => write!(
                f,
                "module was built against runtime {guest:?}, this host implements {host:?}"
            ),
            Mismatch::MajorMismatch { guest, host } => write!(
                f,
                "module needs major version {guest}, this host implements {host}"
            ),
            Mismatch::HostTooOld {
                guest_min_minor,
                host_minor,
            } => write!(
                f,
                "module needs minor version {guest_min_minor} or newer, this host implements {host_minor}"
            ),
        }
    }
}

impl std::error::Error for Mismatch {}

/// The `pathscale.templates` section.
///
/// Carries templates and nothing else — no format version of its own. Each
/// [`Template`] states its own version, first, which is what makes a section
/// holding templates at two versions readable rather than a contradiction. That
/// is not a hypothetical: a migration writes new templates beside old ones, and
/// a section-level version would have to be either the minimum or the maximum
/// and would be a lie in the other direction.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct TemplatesSection {
    /// The templates, in no meaningful order. They are found by
    /// [`crate::template::ContentHash`], not by position.
    #[serde(default)]
    pub templates: Vec<Template>,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn guest(major: u32, min_minor: u32) -> RuntimeSection {
        RuntimeSection {
            name: "solidrs".to_owned(),
            major,
            min_minor,
        }
    }

    fn host(major: u32, minor: u32) -> HostRuntime {
        HostRuntime {
            name: "solidrs".to_owned(),
            major,
            minor,
        }
    }

    #[test]
    fn a_newer_host_links_an_older_module() {
        let linkage = guest(1, 2)
            .link(&host(1, 7))
            .expect("minor 7 offers minor 2");
        assert_eq!(linkage.major(), 1);
        assert_eq!(linkage.minor(), 7);
        assert_eq!(linkage.import_namespace(), "solidrs:1");
    }

    #[test]
    fn an_equal_minor_links() {
        assert!(guest(1, 4).link(&host(1, 4)).is_ok());
    }

    #[test]
    fn an_older_host_is_rejected_before_memory_is_shared() {
        assert_eq!(
            guest(1, 5).link(&host(1, 4)),
            Err(Mismatch::HostTooOld {
                guest_min_minor: 5,
                host_minor: 4
            })
        );
    }

    #[test]
    fn a_major_mismatch_is_rejected_in_both_directions() {
        assert_eq!(
            guest(2, 0).link(&host(1, 9)),
            Err(Mismatch::MajorMismatch { guest: 2, host: 1 })
        );
        assert_eq!(
            guest(1, 0).link(&host(2, 0)),
            Err(Mismatch::MajorMismatch { guest: 1, host: 2 }),
            "a newer host does not silently accept an older major"
        );
    }

    #[test]
    fn a_different_runtime_is_rejected() {
        let other = RuntimeSection {
            name: "somethingelse".to_owned(),
            major: 1,
            min_minor: 0,
        };
        assert!(matches!(
            other.link(&host(1, 0)),
            Err(Mismatch::DifferentRuntime { .. })
        ));
    }

    #[test]
    fn the_import_namespace_round_trips() {
        for major in [0, 1, 2, 41] {
            assert_eq!(
                parse_import_namespace(&import_namespace(major)),
                Some(major)
            );
        }
    }

    #[test]
    fn a_namespace_that_is_not_ours_parses_to_nothing() {
        assert_eq!(parse_import_namespace("blitz"), None);
        assert_eq!(parse_import_namespace("wasi:cli/run@0.2.0"), None);
        // Our prefix, but not a version. Guessing here would link a module
        // against a major it never named.
        assert_eq!(parse_import_namespace("solidrs:next"), None);
        assert_eq!(parse_import_namespace("solidrs:"), None);
        assert_eq!(parse_import_namespace("solidrs:1.2"), None);
    }

    #[test]
    fn the_section_names_are_what_the_module_carries() {
        assert_eq!(RUNTIME_SECTION, "pathscale.runtime");
        assert_eq!(TEMPLATES_SECTION, "pathscale.templates");
    }
}
