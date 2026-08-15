//! The template format: what a compiler writes and a runtime reads.
//!
//! A template is a tree of [`Node`]s with holes in it. The holes are
//! [`BindingId`]s, and that is *all* they are — the format describes the shape
//! of a document and says nothing whatever about how any value in it is
//! produced.
//!
//! # Rule 1: no computation
//!
//! There is no expression type in this module and there must never be one. Not
//! a small one, not "just for string concatenation", not `Concat(Vec<Value>)`.
//!
//! The absence is the feature. A template that cannot compute is a template
//! whose entire behaviour is "put this value in this place", which is what
//! makes presentation separable from behaviour *at runtime* rather than by
//! convention. Add an expression type and the separation becomes a code-review
//! rule, which is to say it stops existing. Every expressive gap this creates
//! is closed the same way: the guest computes the value and writes it to a
//! binding.
//!
//! [`Variant`] looks like the exception and is not, deliberately; see its own
//! documentation.
//!
//! [`Variant`]: AttrValue::Variant

use serde::{Deserialize, Serialize};

/// The version of the template format.
///
/// **Independent of [`HOST_ABI_VERSION`] and [`RUNTIME_ABI_VERSION`].** A
/// change to the calling convention does not invalidate a cached template, and
/// bumping this to describe one would discard every cached artifact to report a
/// change that did not touch them.
///
/// # Rule 3: additive evolution only
///
/// What a bump of this constant is *for* is the case that cannot be handled
/// additively. Almost nothing should need one, because:
///
/// - **New variants are additive.** An old reader rejects an unknown variant
///   loudly, which is the correct outcome: it genuinely cannot render it.
/// - **New struct fields are additive** as long as they carry
///   `#[serde(default)]`, which is why every collection field in this module
///   does.
/// - **Nothing here sets `deny_unknown_fields`,** so a reader older than the
///   writer ignores fields it does not know. That cuts both ways and the cost
///   is stated here rather than discovered later: **a new field is silently
///   dropped by an old reader**, so no field added in the future may be
///   load-bearing for correctness or safety. A field that *must* be honoured is
///   a new variant, or a version bump.
///
/// What is never additive, at any version: reordering a variant so a tag means
/// something else, reusing the name of a removed variant, or changing the
/// meaning of an existing field. The first would silently reinterpret every
/// cached template; the second would do it to exactly the templates written
/// during the window when the name meant the other thing, which is worse
/// because it is intermittent.
///
/// Field *order* is load-bearing for a different reason: it is the canonical
/// traversal that the content hash is taken over. See [`canonical`].
///
/// [`HOST_ABI_VERSION`]: crate::host::HOST_ABI_VERSION
/// [`RUNTIME_ABI_VERSION`]: crate::runtime::RUNTIME_ABI_VERSION
pub const TEMPLATE_FORMAT_VERSION: u32 = 1;

/// A compiled template.
///
/// # Field order
///
/// `version` is first, and it is first in the serialized form because it is
/// first here. A reader that hits an unexpected shape should already know which
/// format it was promised, so that it can say "this is format 4 and I read
/// format 3" instead of "unexpected token at byte 91190".
///
/// # What is not in it
///
/// There is no string table. Names in a template are carried as [`Atom`]s —
/// the strings themselves — rather than as indices into a table the template
/// ships with. A template can arrive from a CDN and be handed to a host whose
/// interner it has never seen, so the numeric [`crate::host::Atom`] ids cannot
/// be baked in; they are issued when the template is loaded. Dropping the
/// intermediate table drops a layer of indirection that no type in this crate
/// could have validated anyway.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Template {
    /// Always [`TEMPLATE_FORMAT_VERSION`] on write. On read it is whatever the
    /// writer had, which is the point of reading it.
    pub version: u32,

    /// The content hash of everything below, and the identity a
    /// [`Component`] reference resolves against.
    ///
    /// Not covered by the hash it contains, for the obvious reason. Neither are
    /// the debug names; see [`canonical`].
    pub hash: ContentHash,

    /// A human-readable name, for diagnostics.
    ///
    /// **Debug only, and excluded from [`Self::hash`].** Renaming a template
    /// does not change its identity, so a rename does not invalidate a cache
    /// and does not break a [`Component`] that refers to it. That property is
    /// the reason the field is called debug rather than name.
    pub name: String,

    /// Every binding this template has holes for.
    ///
    /// Position in this table is *not* identity; [`Binding::id`] is. See
    /// [`BindingId`].
    #[serde(default)]
    pub bindings: Vec<Binding>,

    /// The top-level nodes. More than one, because a template need not have a
    /// single root element and inventing a wrapper to pretend it does would
    /// change the document.
    #[serde(default)]
    pub roots: Vec<Node>,
}

/// A name from a closed vocabulary, in its unresolved form.
///
/// Tag names, attribute names, event names, style property names, and
/// attribute values that come from a set fixed at compile time. In a template
/// these are the strings themselves; crossing the boundary at runtime they are
/// [`crate::host::Atom`], a `u32` into the host's interner. Same concept, two
/// representations, and the load step is the boundary between them.
///
/// # Rule 4: nothing data-derived becomes an atom
///
/// **An atom is never released.** That is the right trade for a name, because
/// names come from a small fixed vocabulary and the table stops growing once
/// the vocabulary is known. It is the wrong trade for anything drawn from the
/// data: a list that scrolls a million rows past would add a million entries
/// the interner can never free.
///
/// So the promise this type makes is not "this is a string". It is "this string
/// comes from a set whose size does not depend on the data". A compiler that
/// puts a per-row value in an `Atom` has not made a type error, it has made a
/// memory leak, and no type in this crate can catch it. Values that vary go in
/// a [`BindingId`], which the guest fills with copied bytes.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct Atom(pub String);

impl Atom {
    /// Borrow the name.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl From<&str> for Atom {
    fn from(s: &str) -> Self {
        Atom(s.to_owned())
    }
}

impl From<String> for Atom {
    fn from(s: String) -> Self {
        Atom(s)
    }
}

impl std::fmt::Display for Atom {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

/// The identity of a hole in the template.
///
/// # Rule 1: no computation
///
/// **A binding is an opaque slot.** It has an id, a kind and a debug name, and
/// no way to say anything about the value that fills it. There is nothing here
/// to hang an expression off, and adding one would be adding the expression
/// type rule 1 forbids by another route.
///
/// # Rule 2: stable ids, not positions
///
/// **Templates ship separately from their guests.** A template is cached, put
/// on a CDN, embedded in a `pathscale.templates` section built at a different
/// time from the guest that reads it. If a binding were "the third hole in
/// document order", then inserting a static `<span>` above it — a change that
/// cannot affect behaviour — would renumber it, and a guest compiled against
/// the old numbering would write the wrong value to the wrong hole with no
/// error anywhere.
///
/// So ids are assigned, they are stable across recompilations that do not
/// change the binding, and they are never reused for a different binding. The
/// position of a [`Binding`] in [`Template::bindings`] carries no meaning.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct BindingId(pub u32);

/// One entry in the binding table.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Binding {
    /// The id referred to from the tree. See [`BindingId`].
    pub id: BindingId,

    /// What sort of slot this is.
    pub kind: BindingKind,

    /// A human-readable name, for diagnostics.
    ///
    /// **Debug only, and excluded from [`Template::hash`].**
    #[serde(default)]
    pub debug: String,
}

/// What sort of slot a [`Binding`] is.
///
/// # This is not computation
///
/// A kind classifies the hole; it does not describe how the value is produced,
/// and there is nothing in it to evaluate. It exists because it is what lets a
/// validator on either side reject `Show { when: <a list> }` before it becomes
/// a rendering bug, and because the two tiers a value can cross in are
/// different for a [`BindingKind::Value`] than for the rest.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum BindingKind {
    /// Dynamic text, or a dynamic attribute or property value.
    ///
    /// Crosses the boundary as copied bytes, never as an atom: this is content,
    /// and content comes from the data. See [`Atom`], rule 4.
    Value,

    /// A boolean. Fills [`Show::when`] and [`AttrValue::Variant::on`].
    Condition,

    /// A list. Fills [`For::each`].
    List,

    /// An event handler. Fills [`EventListener::handler`].
    Handler,
}

/// A node in the template tree.
///
/// # Rule 3
///
/// Variants may be added. No variant may change position in a way that changes
/// what an existing tag means — see [`canonical`] for the tags, and
/// [`TEMPLATE_FORMAT_VERSION`] for the rest of the rule.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum Node {
    /// An element.
    Element(Element),

    /// Static text, shipped in the template.
    ///
    /// Static, but **not** an [`Atom`]: it is content rather than a name, so
    /// interning it would put page copy in a table that is never released. It
    /// crosses in the copied tier like any other text. See [`Atom`], rule 4 —
    /// the rule is about where a string comes from, not about when it is known.
    Literal(String),

    /// Where a [`Component`]'s children are placed.
    ///
    /// Carries nothing. A component's children are in the [`Component`] node
    /// that instantiates it; this marks the hole in the component's own body
    /// that they go into.
    Children,

    /// Conditional content.
    Show(Show),

    /// Repeated content.
    For(For),

    /// An instance of another template.
    Component(Component),
}

/// An element.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Element {
    /// The tag name.
    pub tag: Atom,

    /// Attributes, in author order.
    #[serde(default)]
    pub attrs: Vec<Attribute>,

    /// Inline style declarations, in author order.
    ///
    /// Separate from [`Self::attrs`] rather than a `style` attribute holding a
    /// string, because a style attribute holding a string is a string that
    /// somebody has to build, and building it is computation. Declarations are
    /// a list so that a single one can be bound without rebuilding the rest.
    #[serde(default)]
    pub style: Vec<StyleDecl>,

    /// Event listeners, in registration order.
    ///
    /// Order matters: listeners on one node fire in registration order.
    #[serde(default)]
    pub events: Vec<EventListener>,

    /// Child nodes, in document order.
    #[serde(default)]
    pub children: Vec<Node>,
}

/// One attribute of an [`Element`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Attribute {
    /// Which attribute this is.
    pub target: AttrTarget,

    /// What goes in it.
    pub value: AttrValue,
}

/// Which attribute an [`Attribute`] sets.
///
/// `class` is the unnamed case and everything else is [`Named`]. That is not
/// favouritism: this format is written by a class-recipe compiler, in which
/// `class` is the attribute that composition happens in and the one
/// [`AttrValue::Variant`] exists for. Making it a variant rather than
/// `Named(Atom("class"))` means the common case cannot be misspelled, and means
/// a validator can tell "the class list" from "an attribute that happens to be
/// called class" without a string comparison.
///
/// [`Named`]: AttrTarget::Named
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum AttrTarget {
    /// The `class` attribute.
    Class,

    /// Any other attribute, by name.
    Named(Atom),
}

/// What fills an attribute, a style declaration or a component prop.
///
/// One value type for all three, on purpose. A style value and an attribute
/// value differ in where they are written and in nothing else that this format
/// can see, and two value types would be two things to keep in step through
/// every future addition.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum AttrValue {
    /// A fixed value from a closed set. See [`Atom`] for what "closed" has to
    /// mean here.
    Static(Atom),

    /// The value the guest writes to this binding. Kind [`BindingKind::Value`].
    Bind(BindingId),

    /// A base plus one of two alternatives, chosen by a boolean binding.
    ///
    /// # Why this is not an expression
    ///
    /// It is the one shape that looks like rule 1 being bent, so here is the
    /// line. An expression type is open: it composes, it nests, and once it
    /// exists every future request is a new operator on it. This is closed. It
    /// is three atoms and a boolean, it cannot nest, it cannot grow operands,
    /// and the set of things it can express is fixed forever at "one of two
    /// known class lists".
    ///
    /// It is here because the alternative is worse in a specific way: without
    /// it, a variant class has to be computed by the guest and pushed through a
    /// [`Self::Bind`] on every change, which turns a closed compile-time set of
    /// class strings into per-frame string traffic — the exact thing rule 4 and
    /// the two-tier string design exist to avoid. The values stay atoms and the
    /// boundary stays free.
    ///
    /// If a third alternative is ever needed, it is not a third field on this
    /// variant. It is a new variant, or it is the guest computing it.
    Variant {
        /// Always applied.
        base: Atom,
        /// The boolean that chooses. Kind [`BindingKind::Condition`].
        on: BindingId,
        /// Applied when `on` is true.
        when_true: Atom,
        /// Applied when `on` is false.
        when_false: Atom,
    },
}

/// One inline style declaration.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StyleDecl {
    /// The property name — `display`, `grid-template-columns`. A closed
    /// vocabulary by construction: CSS has a finite list.
    pub property: Atom,

    /// The value.
    ///
    /// [`AttrValue::Static`] is right for a value from a fixed set (`flex`,
    /// `1px`). A value computed per frame — an interpolated width, a colour
    /// from a picker — must be [`AttrValue::Bind`]. Putting it in a
    /// `Static` would intern data. See [`Atom`], rule 4.
    pub value: AttrValue,
}

/// An event listener registered by the template.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EventListener {
    /// The event name — `click`, `input`. Closed vocabulary.
    pub event: Atom,

    /// The handler. Kind [`BindingKind::Handler`].
    ///
    /// A [`BindingId`] rather than an [`AttrValue`], because there is no such
    /// thing as a static handler: the code is on the guest side by definition,
    /// so the only shape available is a binding and offering the other two
    /// would offer shapes with no meaning.
    pub handler: BindingId,

    /// Registration-time flags.
    #[serde(default)]
    pub flags: EventFlags,
}

/// Flags fixed at registration time, not decided per event.
///
/// # Why these are static
///
/// Dispatch is deferred. The host records which listeners matched during
/// propagation, finishes propagating, releases the document, and only then
/// calls the guest — because calling the guest mid-propagation would mean
/// holding a document borrow across a call into guest code, and the guest's
/// first act on a click is to mutate the DOM. See `blitz-wasm`'s ABI.md,
/// "Events: deferred dispatch".
///
/// The consequence is that **a handler cannot cancel an event it has not been
/// called for yet.** By the time it runs, propagation is over. A runtime
/// `stopPropagation()` would therefore be a method that compiles, runs, and
/// does nothing, which is the worse of the two failures.
///
/// Declaring the intent at registration is what makes it expressible at all.
/// The host knows before it propagates, so it can act on it. And this is not
/// hypothetical: a tab close button sitting inside a clickable tab needs
/// exactly this, and needs it to be true of the first click rather than of the
/// second.
///
/// # Rule 3, applied to a flag set
///
/// Named booleans rather than a bitfield, and `#[serde(default)]` so a template
/// written by an older compiler parses. The cost is the one stated at
/// [`TEMPLATE_FORMAT_VERSION`]: **an older reader silently ignores a flag it
/// does not know.** So a flag added here may never be load-bearing for
/// correctness — `stop_propagation` being ignored produces a wrong-looking UI,
/// which is recoverable. Anything whose absence would be unsafe is not a flag.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct EventFlags {
    /// Stop propagating past this node once this listener matches.
    pub stop_propagation: bool,
}

/// Conditional content.
///
/// There is no `otherwise` branch. Adding one later is additive — a
/// `#[serde(default)]` field — and adding it now would be guessing at
/// semantics (does the fallback share the then-branch's scope? does it run its
/// own effects?) with no caller to check the guess against.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Show {
    /// The condition. Kind [`BindingKind::Condition`].
    ///
    /// Nothing in this type says this id occurs in [`Template::bindings`]. See
    /// the crate docs, "What these types do not do".
    pub when: BindingId,

    /// Rendered when the condition holds.
    #[serde(default)]
    pub then: Vec<Node>,
}

/// Repeated content.
///
/// # There is no key here, and that is settled
///
/// `For` carries the list and the body and nothing about row identity. The
/// identity that crosses the boundary is a [`crate::host::RowId`] the guest
/// issues per live row scope. The full argument — why not the key, why not a
/// hash of it, why not an atom — is on that type, because that is the type it
/// is about.
///
/// The part that belongs here is what it means for the *format*: **there is no
/// key binding to add, and adding one would be a regression rather than a
/// feature.** A key in the template says "reconcile by this", which says the
/// host reconciles, which says two independent reconciliations have to agree.
/// They agree only if they run the same algorithm down to duplicate handling
/// and removal order — and `SolidRS`'s `map.rs` permits duplicate keys and
/// chains them through `new_indices_next`. Reconciling on a row id removes that
/// requirement instead of documenting it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct For {
    /// The list. Kind [`BindingKind::List`].
    pub each: BindingId,

    /// Rendered once per row.
    #[serde(default)]
    pub body: Vec<Node>,
}

/// An instance of another template.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Component {
    /// Which template, by content hash.
    ///
    /// By hash rather than by name because a name is a thing two packages can
    /// both have. Nothing here says the hash names a template that exists; the
    /// resolver says that, and it says it at load time on both sides.
    pub of: ContentHash,

    /// The component's source name, for diagnostics.
    ///
    /// **Debug only, and excluded from [`Template::hash`].** Two otherwise
    /// identical trees that name the same component differently are the same
    /// content, which is the property that makes renaming a component free.
    #[serde(default)]
    pub debug: String,

    /// Props passed to the component.
    #[serde(default)]
    pub props: Vec<Prop>,

    /// Attributes applied to the component's root, in author order.
    ///
    /// Distinct from props: a prop is data the component reads, an attribute
    /// lands on the DOM node whatever the component does with its props.
    #[serde(default)]
    pub attrs: Vec<Attribute>,

    /// Children, placed at the component's [`Node::Children`].
    #[serde(default)]
    pub children: Vec<Node>,
}

/// One prop passed to a [`Component`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Prop {
    /// The prop name. A closed vocabulary: it is written in the source.
    pub name: Atom,

    /// The value. The same three shapes as an attribute, and
    /// [`AttrValue::Variant`] means the same thing here — a component that
    /// takes a class list gets one composed the same way.
    pub value: AttrValue,
}

/// The prefix every content hash carries.
pub const HASH_PREFIX: &str = "b3:";

/// The number of hex digits after the prefix: BLAKE3's 256-bit default output.
pub const HASH_HEX_LEN: usize = 64;

/// A content hash: `b3:` followed by 64 lowercase hex digits.
///
/// # The decision, recorded
///
/// **The algorithm is BLAKE3, 256-bit output, lowercase hex, prefixed
/// [`HASH_PREFIX`].** The prefix is not decoration and it is not for a human
/// reader: it is what makes changing the algorithm additive. A hash written by
/// a future compiler reads as `b3s:` or whatever comes next, and an old reader
/// rejects it as an unknown algorithm rather than comparing 64 hex digits of a
/// different function against 64 hex digits of this one and concluding the
/// content differs.
///
/// **What is hashed is the parsed structure, not the encoded bytes.** See
/// [`canonical`] for the traversal and for why the alternative does not work.
///
/// # This type does not compute hashes
///
/// It cannot: a hash function is a dependency, and this crate has one
/// dependency. So it carries the value and validates its *shape* — prefix,
/// length, lowercase hex — and the definition of what to hash lives in
/// [`canonical`] where both sides can implement it identically. A
/// `ContentHash` that parses is a well-formed hash of something. Whether it is
/// the hash of the template carrying it is a thing the reader checks by
/// recomputing, and a thing a reader that has just fetched a template from a
/// CDN should check.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct ContentHash(String);

impl ContentHash {
    /// Check the shape and wrap.
    pub fn parse(value: impl Into<String>) -> Result<Self, ContentHashError> {
        let value = value.into();

        let Some(hex) = value.strip_prefix(HASH_PREFIX) else {
            return Err(ContentHashError::UnknownAlgorithm);
        };
        if hex.len() != HASH_HEX_LEN {
            return Err(ContentHashError::WrongLength { found: hex.len() });
        }
        if let Some(bad) = hex
            .chars()
            .find(|c| !c.is_ascii_hexdigit() || c.is_ascii_uppercase())
        {
            return Err(ContentHashError::NotLowercaseHex { found: bad });
        }

        Ok(ContentHash(value))
    }

    /// The whole thing, prefix included.
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// Just the hex digits.
    pub fn hex(&self) -> &str {
        &self.0[HASH_PREFIX.len()..]
    }
}

impl TryFrom<String> for ContentHash {
    type Error = ContentHashError;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        ContentHash::parse(value)
    }
}

impl From<ContentHash> for String {
    fn from(hash: ContentHash) -> Self {
        hash.0
    }
}

impl std::fmt::Display for ContentHash {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

/// Why a string is not a [`ContentHash`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ContentHashError {
    /// No recognised algorithm prefix. See [`HASH_PREFIX`].
    UnknownAlgorithm,
    /// The right prefix, the wrong number of digits after it.
    WrongLength {
        /// How many were found.
        found: usize,
    },
    /// A digit that is not lowercase hex. Uppercase is rejected rather than
    /// accepted-and-normalised, because two spellings of one hash compare
    /// unequal as strings and this type is used as a key.
    NotLowercaseHex {
        /// The offending character.
        found: char,
    },
}

impl std::fmt::Display for ContentHashError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ContentHashError::UnknownAlgorithm => {
                write!(f, "content hash does not start with {HASH_PREFIX:?}")
            }
            ContentHashError::WrongLength { found } => {
                write!(
                    f,
                    "content hash has {found} hex digits, expected {HASH_HEX_LEN}"
                )
            }
            ContentHashError::NotLowercaseHex { found } => {
                write!(
                    f,
                    "content hash contains {found:?}, which is not a lowercase hex digit"
                )
            }
        }
    }
}

impl std::error::Error for ContentHashError {}

/// The canonical traversal the content hash is taken over.
///
/// # The decision, recorded
///
/// **The hash is over the parsed structure, not over the encoded bytes.**
///
/// The alternative — specify a canonical text form and hash that — was
/// rejected, and the reason is the thing that makes this decision necessary in
/// the first place. A text encoding hashes differently for the same meaning
/// depending on whitespace, on indentation, on how the emitter renders an empty
/// sequence, and on field order if the format admits maps. Pinning all of that
/// down means pinning down an emitter, which means this crate would either
/// depend on one or describe one it cannot check. And it would make the hash
/// depend on the output being text, which the crate docs say nothing may.
///
/// So: hash a structural traversal, defined here, implementable identically on
/// both sides with no shared code and no shared encoder.
///
/// # The traversal
///
/// Depth-first, in declaration order, into a byte stream:
///
/// - **`u32`** — 4 bytes, little-endian.
/// - **`bool`** — one byte, `0` or `1`.
/// - **String** — the byte length as `u32`, then the UTF-8 bytes exactly as
///   authored. No case folding, no Unicode normalisation. Normalising would be
///   a second thing to get identical on two sides, and getting it wrong
///   produces a hash mismatch on text that looks the same, which is the worst
///   failure to debug.
/// - **Sequence** — the element count as `u32`, then each element in order.
///   Order is meaning here: sibling order is document order, attribute order is
///   author order, and listener order is fire order.
/// - **Struct** — its fields in declaration order, with no tag and no field
///   names.
/// - **Enum** — its one-byte tag from this module, then the variant's fields in
///   declaration order.
///
/// # What is excluded
///
/// - [`Template::hash`], which cannot cover itself.
/// - [`Template::name`], [`Binding::debug`] and [`Component::debug`], because
///   they are debug names. Excluding them is what makes renaming free: a rename
///   does not invalidate a cache and does not break a [`Component`] reference.
///   It also means two templates differing only in their names collide in the
///   cache, which is correct — they render identically.
///
/// Everything else is included, [`Template::version`] first.
///
/// # Rule 3, applied to tags
///
/// **A tag is assigned once and never changes.** A new variant takes the next
/// unused number; a removed variant's number is retired, not reused. Reordering
/// the constants below would silently change the hash of every template that
/// uses the affected variant, which invalidates caches rather than corrupting
/// them — but it would also mean two compilers at different versions disagree
/// about a component's identity, which corrupts resolution.
pub mod canonical {
    /// [`Node::Element`](super::Node::Element).
    pub const NODE_ELEMENT: u8 = 1;
    /// [`Node::Literal`](super::Node::Literal).
    pub const NODE_LITERAL: u8 = 2;
    /// [`Node::Children`](super::Node::Children).
    pub const NODE_CHILDREN: u8 = 3;
    /// [`Node::Show`](super::Node::Show).
    pub const NODE_SHOW: u8 = 4;
    /// [`Node::For`](super::Node::For).
    pub const NODE_FOR: u8 = 5;
    /// [`Node::Component`](super::Node::Component).
    pub const NODE_COMPONENT: u8 = 6;

    /// [`AttrTarget::Class`](super::AttrTarget::Class).
    pub const ATTR_TARGET_CLASS: u8 = 1;
    /// [`AttrTarget::Named`](super::AttrTarget::Named).
    pub const ATTR_TARGET_NAMED: u8 = 2;

    /// [`AttrValue::Static`](super::AttrValue::Static).
    pub const ATTR_VALUE_STATIC: u8 = 1;
    /// [`AttrValue::Bind`](super::AttrValue::Bind).
    pub const ATTR_VALUE_BIND: u8 = 2;
    /// [`AttrValue::Variant`](super::AttrValue::Variant).
    pub const ATTR_VALUE_VARIANT: u8 = 3;

    /// [`BindingKind::Value`](super::BindingKind::Value).
    pub const BINDING_KIND_VALUE: u8 = 1;
    /// [`BindingKind::Condition`](super::BindingKind::Condition).
    pub const BINDING_KIND_CONDITION: u8 = 2;
    /// [`BindingKind::List`](super::BindingKind::List).
    pub const BINDING_KIND_LIST: u8 = 3;
    /// [`BindingKind::Handler`](super::BindingKind::Handler).
    pub const BINDING_KIND_HANDLER: u8 = 4;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_content_hash_round_trips_through_its_string_form() {
        let text = format!("{HASH_PREFIX}{}", "ab".repeat(32));
        let hash = ContentHash::parse(text.clone()).expect("well formed");
        assert_eq!(hash.as_str(), text);
        assert_eq!(hash.hex().len(), HASH_HEX_LEN);
        assert_eq!(String::from(hash), text);
    }

    #[test]
    fn a_content_hash_rejects_every_way_of_being_wrong() {
        let hex = "ab".repeat(32);

        assert_eq!(
            ContentHash::parse(hex.clone()),
            Err(ContentHashError::UnknownAlgorithm),
            "no prefix"
        );
        assert_eq!(
            ContentHash::parse(format!("sha256:{hex}")),
            Err(ContentHashError::UnknownAlgorithm),
            "a prefix, but not this algorithm"
        );
        assert_eq!(
            ContentHash::parse(format!("{HASH_PREFIX}abcd")),
            Err(ContentHashError::WrongLength { found: 4 })
        );
        assert_eq!(
            ContentHash::parse(format!("{HASH_PREFIX}{}", "AB".repeat(32))),
            Err(ContentHashError::NotLowercaseHex { found: 'A' }),
            "uppercase is rejected, not normalised: this type is a key"
        );
        assert_eq!(
            ContentHash::parse(format!("{HASH_PREFIX}{}z", &hex[1..])),
            Err(ContentHashError::NotLowercaseHex { found: 'z' })
        );
    }

    #[test]
    fn canonical_tags_are_distinct_within_each_enum() {
        use canonical::*;

        let nodes = [
            NODE_ELEMENT,
            NODE_LITERAL,
            NODE_CHILDREN,
            NODE_SHOW,
            NODE_FOR,
            NODE_COMPONENT,
        ];
        let mut seen = nodes.to_vec();
        seen.sort_unstable();
        seen.dedup();
        assert_eq!(seen.len(), nodes.len(), "two node variants share a tag");

        // A tag of zero would be indistinguishable from a zero byte written by
        // something that forgot to write a tag at all.
        assert!(nodes.iter().all(|&t| t != 0));
        assert!(
            [ATTR_TARGET_CLASS, ATTR_TARGET_NAMED]
                .iter()
                .all(|&t| t != 0)
        );
        assert!(
            [ATTR_VALUE_STATIC, ATTR_VALUE_BIND, ATTR_VALUE_VARIANT]
                .iter()
                .all(|&t| t != 0)
        );
        assert!(
            [
                BINDING_KIND_VALUE,
                BINDING_KIND_CONDITION,
                BINDING_KIND_LIST,
                BINDING_KIND_HANDLER
            ]
            .iter()
            .all(|&t| t != 0)
        );
    }
}
