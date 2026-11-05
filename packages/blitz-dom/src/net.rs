#[path = "web_fonts.rs"]
pub mod web_fonts;

use blitz_traits::handoff::EventTarget;
use blitz_traits::node_id::NodeId;
use selectors::context::QuirksMode;
use std::sync::atomic::Ordering as Ao;
use std::{
    io::Cursor,
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicUsize},
    },
};
use style::{
    font_face::{FontFaceSourceFormat, FontFaceSourceFormatKeyword, FontStyleRange, Source},
    media_queries::MediaList,
    servo_arc::Arc as ServoArc,
    shared_lock::SharedRwLock,
    shared_lock::{Locked, SharedRwLockReadGuard},
    stylesheets::{
        AllowImportRules, CssRule, DocumentStyleSheet, ImportRule, Origin, Stylesheet,
        StylesheetInDocument, StylesheetLoader as ServoStylesheetLoader, UrlExtraData,
        import_rule::{ImportLayer, ImportSheet, ImportSupportsCondition},
    },
    values::{CssUrl, SourceLocation},
};

use blitz_traits::net::{AbortSignal, Bytes, NetHandler, NetProvider, Request, RequestDestination};
use blitz_traits::shell::ShellProvider;

use url::Url;

use crate::{document::DocumentEvent, util::ImageType};

pub(crate) fn stamped_request(url: Url, signal: Option<&AbortSignal>) -> Request {
    let mut req = Request::get(url);
    if let Some(sig) = signal {
        req = req.signal(sig.clone());
    }
    req
}

/// Add the `Referer` a browser sends for a subresource under the default
/// `strict-origin-when-cross-origin` policy: the full URL (minus fragment and
/// credentials) to the same origin, only the origin across origins, nothing
/// on an HTTPS to HTTP downgrade or from a non-HTTP document. CDNs commonly
/// refuse image requests without one.
pub(crate) fn with_referrer(mut req: Request, document: &Url) -> Request {
    if !matches!(document.scheme(), "http" | "https") {
        return req;
    }
    if document.scheme() == "https" && req.url.scheme() == "http" {
        return req;
    }
    let referrer = if document.origin() == req.url.origin() {
        let mut full = document.clone();
        full.set_fragment(None);
        let _ = full.set_username("");
        let _ = full.set_password(None);
        full.to_string()
    } else {
        format!("{}/", document.origin().ascii_serialization())
    };
    if let Ok(value) = blitz_traits::net::http::HeaderValue::from_str(&referrer) {
        req.headers
            .insert(blitz_traits::net::http::header::REFERER, value);
    }
    req
}

/// Carries `@font-face` descriptors from CSS parsing through to font
/// registration so `parley::Collection::register_fonts` can alias the bytes
/// under the `font-family` declared in CSS rather than whatever family name
/// the TTF's own `name` table reports.
///
/// All fields are `Option` because each descriptor is independently optional
/// at the CSS level. Missing fields fall back to the values parley reads
/// from the font's own metadata.
#[derive(Clone, Debug, Default)]
pub struct FontFaceOverrides {
    /// `font-family` descriptor (the alias the rest of the stylesheet uses).
    pub family_name: Option<String>,
    /// `font-weight` descriptor as a single CSS weight (100–900). Stylo
    /// parses this as a range; we record the lower bound, which equals the
    /// upper bound in the common single-value case.
    pub weight: Option<f32>,
    /// `font-style` descriptor mapped to fontique's `FontStyle`.
    pub style: Option<parley::fontique::FontStyle>,
}

#[derive(Clone, Debug)]
pub enum Resource {
    Image(ImageType, u32, u32, Arc<Vec<u8>>),
    #[cfg(feature = "svg")]
    Svg(ImageType, crate::node::SvgImageData),
    Css(DocumentStyleSheet),
    Font(Bytes, FontFaceOverrides),
    TrackedFont(Bytes, FontFaceOverrides, Arc<web_fonts::WebFont>),
    /// HTML fetched for an `<iframe>` element's `src`
    DocumentSrc(String),
    /// A stylesheet fetched for an `@import` rule, and the rule it belongs to.
    ///
    /// Carried back rather than attached where it was parsed. Attaching means
    /// taking stylo's document-wide `SharedRwLock` for writing, and this
    /// arrives on a network worker while the document thread may be holding
    /// the same lock for reading to resolve style. That lock is an
    /// `AtomicRefCell`, so the two do not queue: the writer panics with
    /// "already immutably borrowed" and takes the page with it.
    ImportSheet(ServoArc<Locked<ImportRule>>, ServoArc<Stylesheet>),
    None,
}

pub(crate) struct ResourceHandler<T: Send + Sync + 'static> {
    doc_id: usize,
    request_id: usize,
    node_id: Option<NodeId>,
    request_url: Option<String>,
    tx: EventTarget<DocumentEvent>,
    shell_provider: Arc<dyn ShellProvider>,
    /// Set once `respond` has delivered a result. A fetch that never calls
    /// `bytes` drops the handler; without this the critical-resource id stays
    /// registered and painting never starts.
    settled: AtomicBool,
    data: T,
}

impl<T: Send + Sync + 'static> ResourceHandler<T> {
    pub(crate) fn new(
        tx: EventTarget<DocumentEvent>,
        doc_id: usize,
        node_id: Option<NodeId>,
        shell_provider: Arc<dyn ShellProvider>,
        data: T,
    ) -> Self {
        Self::new_with_url(tx, doc_id, node_id, shell_provider, data, None)
    }

    fn new_with_url(
        tx: EventTarget<DocumentEvent>,
        doc_id: usize,
        node_id: Option<NodeId>,
        shell_provider: Arc<dyn ShellProvider>,
        data: T,
        request_url: Option<String>,
    ) -> Self {
        static REQUEST_ID_COUNTER: AtomicUsize = AtomicUsize::new(0);
        Self {
            request_id: REQUEST_ID_COUNTER.fetch_add(1, Ao::Relaxed),
            doc_id,
            node_id,
            request_url,
            tx,
            shell_provider,
            settled: AtomicBool::new(false),
            data,
        }
    }

    pub(crate) fn boxed(
        tx: EventTarget<DocumentEvent>,
        doc_id: usize,
        node_id: Option<NodeId>,
        shell_provider: Arc<dyn ShellProvider>,
        data: T,
    ) -> Box<dyn NetHandler>
    where
        ResourceHandler<T>: NetHandler,
    {
        Box::new(Self::new(tx, doc_id, node_id, shell_provider, data)) as _
    }

    pub(crate) fn boxed_for_url(
        tx: EventTarget<DocumentEvent>,
        doc_id: usize,
        node_id: Option<NodeId>,
        shell_provider: Arc<dyn ShellProvider>,
        data: T,
        request_url: String,
    ) -> Box<dyn NetHandler>
    where
        ResourceHandler<T>: NetHandler,
    {
        Box::new(Self::new_with_url(
            tx,
            doc_id,
            node_id,
            shell_provider,
            data,
            Some(request_url),
        )) as _
    }

    pub(crate) fn request_id(&self) -> usize {
        self.request_id
    }

    fn respond(&self, resolved_url: String, result: Result<Resource, String>) {
        self.settled.store(true, Ao::Release);
        let response = ResourceLoadResponse {
            request_id: self.request_id,
            node_id: self.node_id,
            resolved_url: Some(resolved_url),
            result,
        };
        let _ = self
            .tx
            .publish(Arc::new(DocumentEvent::ResourceLoad(response)));
    }
}

impl<T: Send + Sync + 'static> Drop for ResourceHandler<T> {
    fn drop(&mut self) {
        if self.settled.load(Ao::Acquire) {
            return;
        }
        let _ = self.tx.publish(Arc::new(DocumentEvent::ResourceLoad(
            ResourceLoadResponse {
                request_id: self.request_id,
                node_id: self.node_id,
                resolved_url: self.request_url.clone(),
                result: Err(String::from("resource fetch failed")),
            },
        )));
    }
}

#[allow(unused)]
pub struct ResourceLoadResponse {
    pub request_id: usize,
    pub node_id: Option<NodeId>,
    pub resolved_url: Option<String>,
    pub result: Result<Resource, String>,
}

pub struct StylesheetHandler {
    pub source_url: Url,
    pub guard: SharedRwLock,
    pub net_provider: Arc<dyn NetProvider>,
    pub abort_signal: Option<AbortSignal>,
}

impl NetHandler for ResourceHandler<StylesheetHandler> {
    fn bytes(self: Box<Self>, resolved_url: String, bytes: Bytes) {
        let Ok(css) = std::str::from_utf8(&bytes) else {
            return self.respond(resolved_url, Err(String::from("Invalid UTF8")));
        };

        // NOTE(Nico): I don't *think* external stylesheets should have HTML entities escaped
        // let escaped_css = html_escape::decode_html_entities(css);

        let sheet = Stylesheet::from_str(
            css,
            self.data.source_url.clone().into(),
            Origin::Author,
            ServoArc::new(self.data.guard.wrap(MediaList::empty())),
            self.data.guard.clone(),
            Some(&StylesheetLoader {
                tx: self.tx.clone(),
                doc_id: self.doc_id,
                net_provider: self.data.net_provider.clone(),
                shell_provider: self.shell_provider.clone(),
                abort_signal: self.data.abort_signal.clone(),
            }),
            None, // error_reporter
            QuirksMode::NoQuirks,
            AllowImportRules::Yes,
        );

        self.respond(
            resolved_url,
            Ok(Resource::Css(DocumentStyleSheet(ServoArc::new(sheet)))),
        );
    }
}

#[derive(Clone)]
pub(crate) struct StylesheetLoader {
    pub(crate) tx: EventTarget<DocumentEvent>,
    pub(crate) doc_id: usize,
    pub(crate) net_provider: Arc<dyn NetProvider>,
    pub(crate) shell_provider: Arc<dyn ShellProvider>,
    pub(crate) abort_signal: Option<AbortSignal>,
}
impl ServoStylesheetLoader for StylesheetLoader {
    fn request_stylesheet(
        &self,
        url: CssUrl,
        location: SourceLocation,
        lock: &SharedRwLock,
        media: ServoArc<Locked<MediaList>>,
        supports: Option<ImportSupportsCondition>,
        layer: ImportLayer,
    ) -> ServoArc<Locked<ImportRule>> {
        if !supports.as_ref().is_none_or(|s| s.enabled) {
            return ServoArc::new(lock.wrap(ImportRule {
                url,
                stylesheet: ImportSheet::new_refused(),
                supports,
                layer,
                source_location: location,
            }));
        }

        let import = ImportRule {
            url,
            stylesheet: ImportSheet::new_pending(),
            supports,
            layer,
            source_location: location,
        };

        let url = import.url.url().unwrap().clone();
        let import = ServoArc::new(lock.wrap(import));
        self.net_provider.fetch(
            self.doc_id,
            stamped_request(url.as_ref().clone(), self.abort_signal.as_ref())
                .destination(RequestDestination::Style),
            ResourceHandler::boxed(
                self.tx.clone(),
                self.doc_id,
                None, // node_id
                self.shell_provider.clone(),
                NestedStylesheetHandler {
                    url: url.clone(),
                    loader: self.clone(),
                    lock: lock.clone(),
                    media,
                    import_rule: import.clone(),
                },
            ),
        );

        import
    }
}

struct NestedStylesheetHandler {
    loader: StylesheetLoader,
    lock: SharedRwLock,
    url: ServoArc<Url>,
    media: ServoArc<Locked<MediaList>>,
    import_rule: ServoArc<Locked<ImportRule>>,
}

impl NetHandler for ResourceHandler<NestedStylesheetHandler> {
    fn bytes(self: Box<Self>, resolved_url: String, bytes: Bytes) {
        let Ok(css) = std::str::from_utf8(&bytes) else {
            return self.respond(resolved_url, Err(String::from("Invalid UTF8")));
        };

        // NOTE(Nico): I don't *think* external stylesheets should have HTML entities escaped
        // let escaped_css = html_escape::decode_html_entities(css);

        let sheet = ServoArc::new(Stylesheet::from_str(
            css,
            UrlExtraData(self.data.url.clone()),
            Origin::Author,
            self.data.media.clone(),
            self.data.lock.clone(),
            Some(&self.data.loader),
            None, // error_reporter
            QuirksMode::NoQuirks,
            AllowImportRules::Yes,
        ));

        // `@font-face` is scanned by the document thread too, for the same
        // reason the attach is: reading the sheet needs this lock, the document
        // thread writes it while styling, and an `AtomicRefCell` panics rather
        // than waiting. Doing it here left a read on a network worker racing a
        // write on the document thread — the same defect one line further down,
        // and it would have panicked the same way.
        let import_rule = self.data.import_rule.clone();
        self.respond(resolved_url, Ok(Resource::ImportSheet(import_rule, sheet)))
    }
}

struct FontFaceHandler {
    format: FontFaceSourceFormatKeyword,
    overrides: FontFaceOverrides,
    ticket: Option<Arc<web_fonts::WebFont>>,
    queued: bool,
}
impl Drop for FontFaceHandler {
    fn drop(&mut self) {
        if !self.queued {
            if let Some(ticket) = &self.ticket {
                ticket.finish(false);
            }
        }
    }
}
impl NetHandler for ResourceHandler<FontFaceHandler> {
    fn bytes(mut self: Box<Self>, resolved_url: String, bytes: Bytes) {
        let result = self.data.parse(bytes);
        let result = match (result, self.data.ticket.as_ref()) {
            (Ok(Resource::Font(bytes, overrides)), Some(ticket)) => {
                self.data.queued = true;
                Ok(Resource::TrackedFont(bytes, overrides, Arc::clone(ticket)))
            }
            (Ok(Resource::None), Some(_)) => Err(String::from("Unsupported font data")),
            (result, _) => result,
        };
        self.respond(resolved_url, result)
    }
}
impl FontFaceHandler {
    fn parse(&mut self, bytes: Bytes) -> Result<Resource, String> {
        if self.format == FontFaceSourceFormatKeyword::None && bytes.len() >= 4 {
            self.format = match &bytes.as_ref()[0..4] {
                // WOFF (v1) files begin with 0x774F4646 ('wOFF' in ascii)
                // See: <https://w3c.github.io/woff/woff1/spec/Overview.html#WOFFHeader>
                #[cfg(feature = "woff")]
                b"wOFF" => FontFaceSourceFormatKeyword::Woff,
                // WOFF2 files begin with 0x774F4632 ('wOF2' in ascii)
                // See: <https://w3c.github.io/woff/woff2/#woff20Header>
                #[cfg(feature = "woff")]
                b"wOF2" => FontFaceSourceFormatKeyword::Woff2,
                // Opentype fonts with CFF data begin with 0x4F54544F ('OTTO' in ascii)
                // See: <https://learn.microsoft.com/en-us/typography/opentype/spec/otff#organization-of-an-opentype-font>
                b"OTTO" => FontFaceSourceFormatKeyword::Opentype,
                // Opentype fonts truetype outlines begin with 0x00010000
                // See: <https://learn.microsoft.com/en-us/typography/opentype/spec/otff#organization-of-an-opentype-font>
                &[0x00, 0x01, 0x00, 0x00] => FontFaceSourceFormatKeyword::Truetype,
                // Truetype fonts begin with 0x74727565 ('true' in ascii)
                // See: <https://developer.apple.com/fonts/TrueType-Reference-Manual/RM06/Chap6.html#ScalerTypeNote>
                b"true" => FontFaceSourceFormatKeyword::Truetype,
                _ => FontFaceSourceFormatKeyword::None,
            }
        }

        // Satisfy rustc's mutability linting with woff feature both enabled/disabled
        #[cfg(feature = "woff")]
        let mut bytes = bytes;

        match self.format {
            #[cfg(feature = "woff")]
            FontFaceSourceFormatKeyword::Woff => {
                #[cfg(feature = "tracing")]
                tracing::info!("Decompressing woff1 font");

                // Use wuff crate to decompress font
                let decompressed = wuff::decompress_woff1(&bytes).ok();

                if let Some(decompressed) = decompressed {
                    bytes = Bytes::from(decompressed);
                } else {
                    #[cfg(feature = "tracing")]
                    tracing::warn!("Failed to decompress woff1 font");
                }
            }
            #[cfg(feature = "woff")]
            FontFaceSourceFormatKeyword::Woff2 => {
                #[cfg(feature = "tracing")]
                tracing::info!("Decompressing woff2 font");

                // Use wuff crate to decompress font
                let decompressed = wuff::decompress_woff2(&bytes).ok();

                if let Some(decompressed) = decompressed {
                    bytes = Bytes::from(decompressed);
                } else {
                    #[cfg(feature = "tracing")]
                    tracing::warn!("Failed to decompress woff2 font");
                }
            }
            FontFaceSourceFormatKeyword::None => {
                // Should this be an error?
                return Ok(Resource::None);
            }
            _ => {}
        }

        Ok(Resource::Font(bytes, std::mem::take(&mut self.overrides)))
    }
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn fetch_font_face(
    tx: EventTarget<DocumentEvent>,
    doc_id: usize,
    node_id: Option<NodeId>,
    sheet: &Stylesheet,
    network_provider: &Arc<dyn NetProvider>,
    shell_provider: &Arc<dyn ShellProvider>,
    read_guard: &SharedRwLockReadGuard,
    abort_signal: Option<&AbortSignal>,
    fonts: &mut Vec<Arc<web_fonts::WebFont>>,
    epoch: usize,
) {
    sheet
        .contents(read_guard)
        .rules(read_guard)
        .iter()
        .filter_map(|rule| match rule {
            CssRule::FontFace(font_face) => {
                let descriptor = &font_face.read_with(read_guard).descriptors;
                let family = descriptor.font_family.as_ref()?;
                let src = descriptor.src.as_ref()?;
                // Capture the @font-face descriptors so parley can register
                // the font under the CSS-declared family name (and weight /
                // style) rather than whatever metadata the TTF reports.
                let overrides = FontFaceOverrides {
                    family_name: Some(family.name.to_string()),
                    weight: descriptor
                        .font_weight
                        .as_ref()
                        .and_then(|range| range.0.compute().map(|w| w.value())),
                    style: descriptor.font_style.as_ref().map(stylo_to_fontique_style),
                };
                Some((src, overrides))
            }
            _ => None,
        })
        .for_each(|(source_list, overrides)| {
            // Find the first font source in the source list that specifies a font of a type
            // that we support.
            let preferred_source = source_list
                .0
                .iter()
                .filter_map(|source| match source {
                    Source::Url(url_source) => Some(url_source),
                    // TODO: support local fonts in @font-face
                    Source::Local(_) => None,
                })
                .find_map(|url_source| {
                    let mut format = match &url_source.format_hint {
                        Some(FontFaceSourceFormat::Keyword(fmt)) => *fmt,
                        Some(FontFaceSourceFormat::String(str)) => match str.as_str() {
                            "woff2" => FontFaceSourceFormatKeyword::Woff2,
                            "ttf" => FontFaceSourceFormatKeyword::Truetype,
                            "otf" => FontFaceSourceFormatKeyword::Opentype,
                            _ => FontFaceSourceFormatKeyword::None,
                        },
                        _ => FontFaceSourceFormatKeyword::None,
                    };
                    if format == FontFaceSourceFormatKeyword::None {
                        let (_, end) = url_source.url.as_str().rsplit_once('.')?;
                        format = match end {
                            "woff2" => FontFaceSourceFormatKeyword::Woff2,
                            "woff" => FontFaceSourceFormatKeyword::Woff,
                            "ttf" => FontFaceSourceFormatKeyword::Truetype,
                            "otf" => FontFaceSourceFormatKeyword::Opentype,
                            "svg" => FontFaceSourceFormatKeyword::Svg,
                            "eot" => FontFaceSourceFormatKeyword::EmbeddedOpentype,
                            _ => FontFaceSourceFormatKeyword::None,
                        }
                    }

                    if matches!(
                        format,
                        FontFaceSourceFormatKeyword::Svg
                            | FontFaceSourceFormatKeyword::EmbeddedOpentype
                    ) {
                        #[cfg(feature = "tracing")]
                        tracing::warn!("Skipping unsupported font of type {:?}", format);
                        return None;
                    }

                    #[cfg(not(feature = "woff"))]
                    if matches!(
                        format,
                        FontFaceSourceFormatKeyword::Woff | FontFaceSourceFormatKeyword::Woff2
                    ) {
                        #[cfg(feature = "tracing")]
                        tracing::warn!("Skipping unsupported font of type {:?}", format);
                        return None;
                    }

                    // A relative url with no base url to resolve against
                    // yields None; skip the source instead of panicking
                    let Some(url) = url_source.url.url() else {
                        #[cfg(feature = "tracing")]
                        tracing::warn!("Skipping @font-face source with unresolvable url");
                        return None;
                    };
                    let url = url.as_ref().clone();
                    Some((url, format))
                });

            if let Some((url, format)) = preferred_source {
                let ticket = web_fonts::WebFont::new(
                    overrides.clone(),
                    Some(url.clone()),
                    None,
                    true,
                    epoch,
                    shell_provider.clone(),
                );
                fonts.push(Arc::clone(&ticket));
                network_provider.fetch(
                    doc_id,
                    stamped_request(url, abort_signal).destination(RequestDestination::Font),
                    ResourceHandler::boxed(
                        tx.clone(),
                        doc_id,
                        node_id,
                        shell_provider.clone(),
                        FontFaceHandler {
                            format,
                            overrides,
                            ticket: Some(ticket),
                            queued: false,
                        },
                    ),
                );
            }
        })
}

/// Translate stylo's `@font-face` `font-style` descriptor into the fontique
/// `FontStyle` enum used by parley. Stylo encodes Italic and Oblique-with-
/// angle distinctly; CSS's bare `normal` is parsed as `Oblique(0deg, 0deg)`
/// by stylo (see the `FontStyle::parse` impl in stylo's `font_face.rs`), so
/// that pattern is treated as `Normal` here.
fn load_script_font(doc: &mut crate::BaseDocument, face: &Arc<web_fonts::WebFont>) {
    if !face.start() {
        return;
    }
    let mut handler = FontFaceHandler {
        format: FontFaceSourceFormatKeyword::None,
        overrides: face.overrides.clone(),
        ticket: Some(Arc::clone(face)),
        queued: false,
    };
    if let Some(bytes) = &face.data {
        match handler.parse(bytes.clone()) {
            Ok(Resource::Font(bytes, overrides)) => {
                handler.queued = true;
                doc.load_resource(ResourceLoadResponse {
                    request_id: usize::MAX,
                    node_id: None,
                    resolved_url: None,
                    result: Ok(Resource::TrackedFont(bytes, overrides, Arc::clone(face))),
                });
            }
            _ => face.finish(false),
        }
    } else if let Some(url) = &face.source {
        doc.net_provider.fetch(
            crate::Document::id(doc),
            stamped_request(url.clone(), doc.abort_signal.as_ref())
                .destination(RequestDestination::Font),
            ResourceHandler::boxed(
                doc.tx.clone(),
                crate::Document::id(doc),
                None,
                doc.shell_provider.clone(),
                handler,
            ),
        );
    } else {
        face.finish(false);
    }
}

fn stylo_to_fontique_style(style: &FontStyleRange) -> parley::fontique::FontStyle {
    use parley::fontique::FontStyle as Fq;
    match style {
        FontStyleRange::Italic => Fq::Italic,
        FontStyleRange::Oblique(min, max) => {
            let angle = min.degrees();
            // Stylo emits `Oblique(0deg, 0deg)` for the literal CSS `normal`
            // keyword. Map that back to `Normal` so parley's font matching
            // doesn't misclassify upright fonts.
            if angle.is_none_or(|a| a == 0.0) && max.degrees().is_none_or(|a| a == 0.0) {
                Fq::Normal
            } else {
                Fq::Oblique(angle)
            }
        }
    }
}

/// Handles HTML fetched for an `<iframe>` element's `src`
pub(crate) struct DocumentSrcHandler;

impl NetHandler for ResourceHandler<DocumentSrcHandler> {
    fn bytes(self: Box<Self>, resolved_url: String, bytes: Bytes) {
        let html = String::from_utf8_lossy(&bytes).into_owned();
        self.respond(resolved_url, Ok(Resource::DocumentSrc(html)));
    }
}

pub struct ImageHandler {
    kind: ImageType,
}
impl ImageHandler {
    pub fn new(kind: ImageType) -> Self {
        Self { kind }
    }
}

impl NetHandler for ResourceHandler<ImageHandler> {
    fn bytes(self: Box<Self>, resolved_url: String, bytes: Bytes) {
        let result = self.data.parse(bytes);
        self.respond(resolved_url, result)
    }
}

/// A JPEG XL codestream starts with FF 0A; the container form starts with the 12-byte "JXL " box.
#[cfg(feature = "image-formats")]
pub fn is_jxl(bytes: &[u8]) -> bool {
    const CONTAINER: [u8; 12] = [
        0, 0, 0, 0x0C, b'J', b'X', b'L', b' ', 0x0D, 0x0A, 0x87, 0x0A,
    ];
    bytes.starts_with(&[0xFF, 0x0A]) || bytes.starts_with(&CONTAINER)
}

/// The first keyframe of a JPEG XL image in sRGB, as RGBA8. Embedders decode
/// script and canvas images through this too, so there is one JXL path.
#[cfg(feature = "image-formats")]
pub fn decode_jxl(bytes: &[u8]) -> Result<(u32, u32, Vec<u8>), String> {
    use jxl::api::{
        Endianness, Event, ExtraChannel, JxlColorEncoding, JxlColorProfile, JxlColorType,
        JxlDataFormat, JxlDecoder, JxlDecoderOptions, JxlOutputBuffer, JxlPixelFormat,
    };
    use jxl_cms::{JxlCms, moxcms::MoxCms};

    // Refuse oversized images before rendering. The float planes used below cost
    // several times the RGBA8 output, so budget dimensions at 64 bytes per pixel.
    const ALLOCATION_LIMIT: u64 = 64 * 1024 * 1024;
    const BYTES_PER_PIXEL_BUDGET: u64 = 64;
    let mut options = JxlDecoderOptions::default();
    options.sample_limit = Some(
        usize::try_from(ALLOCATION_LIMIT)
            .map_err(|_| "JPEG XL sample limit exceeds usize".to_owned())?
            / std::mem::size_of::<f32>(),
    );
    let mut decoder = JxlDecoder::new(options);
    let mut input = bytes;
    loop {
        match decoder
            .process(&mut input, None, None)
            .map_err(|error| error.to_string())?
        {
            Event::BasicInfo => break,
            Event::NeedMoreInput { .. } => return Err("truncated JPEG XL input".into()),
            _ => return Err("JPEG XL decoder did not produce image information".into()),
        }
    }

    let info = decoder
        .basic_info()
        .cloned()
        .ok_or_else(|| "JPEG XL basic information is unavailable".to_owned())?;
    let width = u32::try_from(info.size.0).map_err(|_| "JPEG XL width exceeds u32".to_owned())?;
    let height = u32::try_from(info.size.1).map_err(|_| "JPEG XL height exceeds u32".to_owned())?;
    if info.size.0 == 0 || info.size.1 == 0 {
        return Err("JPEG XL dimensions must be positive".into());
    }
    let pixels = info
        .size
        .0
        .checked_mul(info.size.1)
        .ok_or_else(|| "JPEG XL pixel count overflows usize".to_owned())?;
    let estimated_working_set = u64::try_from(pixels)
        .ok()
        .and_then(|pixels| pixels.checked_mul(BYTES_PER_PIXEL_BUDGET))
        .ok_or_else(|| "JPEG XL working-set estimate overflows u64".to_owned())?;
    if estimated_working_set > ALLOCATION_LIMIT {
        return Err(format!("{width}x{height} exceeds the decode budget"));
    }
    let rgba_bytes = u64::try_from(pixels)
        .ok()
        .and_then(|pixels| pixels.checked_mul(4))
        .ok_or_else(|| "JPEG XL decoded size overflows u64".to_owned())?;
    debug_assert!(rgba_bytes <= ALLOCATION_LIMIT);

    let grayscale = decoder
        .current_pixel_format()
        .ok_or_else(|| "JPEG XL pixel format is unavailable".to_owned())?
        .color_type
        .is_grayscale();
    let alpha_index = info
        .extra_channels
        .iter()
        .position(|channel| channel.ec_type == ExtraChannel::Alpha);
    let initial_profile = decoder
        .output_color_profile()
        .cloned()
        .ok_or_else(|| "JPEG XL color profile is unavailable".to_owned())?;
    let cmyk = initial_profile.is_cmyk();
    let black_index = info
        .extra_channels
        .iter()
        .position(|channel| channel.ec_type == ExtraChannel::Black);
    if cmyk && black_index.is_none() {
        return Err("CMYK JPEG XL image is missing its black channel".into());
    }

    // Keep the main alpha interleaved for RGB/gray images. CMYK has no
    // CMYKA pixel format, so request alpha as a separate plane in that case.
    let color_type = if cmyk {
        JxlColorType::Cmyk
    } else {
        match (grayscale, alpha_index.is_some()) {
            (true, true) => JxlColorType::GrayscaleAlpha,
            (true, false) => JxlColorType::Grayscale,
            (false, true) => JxlColorType::Rgba,
            (false, false) => JxlColorType::Rgb,
        }
    };
    let separate_alpha = if cmyk { alpha_index } else { None };
    let f32_format = JxlDataFormat::F32 {
        endianness: Endianness::native(),
    };
    let pixel_format = JxlPixelFormat {
        color_type,
        color_data_format: Some(f32_format),
        extra_channel_format: info
            .extra_channels
            .iter()
            .enumerate()
            .map(|(index, _)| (Some(index) == separate_alpha).then_some(f32_format))
            .collect(),
    };
    decoder
        .set_pixel_format(pixel_format)
        .map_err(|error| error.to_string())?;
    let source_profile = decoder
        .output_color_profile()
        .cloned()
        .ok_or_else(|| "JPEG XL output color profile is unavailable".to_owned())?;

    let channels = color_type.samples_per_pixel();
    let color_sample_count = pixels
        .checked_mul(channels)
        .ok_or_else(|| "JPEG XL color sample count overflows usize".to_owned())?;
    let mut color_samples = vec![0.0f32; color_sample_count];
    let mut alpha_samples = separate_alpha.map(|_| vec![0.0f32; pixels]);
    let row_bytes = info
        .size
        .0
        .checked_mul(channels)
        .and_then(|samples| samples.checked_mul(std::mem::size_of::<f32>()))
        .ok_or_else(|| "JPEG XL output row size overflows usize".to_owned())?;
    let mut output_buffers = vec![JxlOutputBuffer::new(
        bytemuck::cast_slice_mut(&mut color_samples),
        info.size.1,
        row_bytes,
    )];
    if let Some(alpha_samples) = &mut alpha_samples {
        let alpha_bytes = info
            .size
            .0
            .checked_mul(std::mem::size_of::<f32>())
            .ok_or_else(|| "JPEG XL alpha row size overflows usize".to_owned())?;
        output_buffers.push(JxlOutputBuffer::new(
            bytemuck::cast_slice_mut(alpha_samples),
            info.size.1,
            alpha_bytes,
        ));
    }

    loop {
        match decoder
            .process(&mut input, Some(&mut output_buffers), None)
            .map_err(|error| error.to_string())?
        {
            Event::FrameHeader => {}
            Event::FrameComplete { .. } => break,
            Event::NeedMoreInput { .. } => return Err("truncated JPEG XL frame".into()),
            _ => return Err("JPEG XL image has no visible frame".into()),
        }
    }
    drop(output_buffers);

    let target_profile = JxlColorProfile::Simple(JxlColorEncoding::srgb(false));
    let mut transforms = if !grayscale && !cmyk && source_profile == target_profile {
        None
    } else {
        let (output_channels, mut transforms) = MoxCms
            .initialize_transforms(1, info.size.0, source_profile, target_profile, 255.0)
            .map_err(|error| error.to_string())?;
        if output_channels != 3 || transforms.len() != 1 {
            return Err("JPEG XL CMS did not provide one RGB transform".into());
        }
        Some(transforms.remove(0))
    };

    let input_color_channels = if cmyk {
        4
    } else if grayscale {
        1
    } else {
        3
    };
    let row_samples = info
        .size
        .0
        .checked_mul(input_color_channels)
        .ok_or_else(|| "JPEG XL input row sample count overflows usize".to_owned())?;
    let mut input_row = vec![0.0f32; row_samples];
    let output_row_samples = info
        .size
        .0
        .checked_mul(3)
        .ok_or_else(|| "JPEG XL output row sample count overflows usize".to_owned())?;
    let mut output_row = vec![0.0f32; output_row_samples];
    let rgba_len =
        usize::try_from(rgba_bytes).map_err(|_| "JPEG XL output size exceeds usize".to_owned())?;
    let mut rgba = vec![0u8; rgba_len];
    let to_byte = |value: f32| (value.clamp(0.0, 1.0) * 255.0 + 0.5) as u8;
    for y in 0..info.size.1 {
        for x in 0..info.size.0 {
            let pixel = y * info.size.0 + x;
            let source = pixel * channels;
            let input = x * input_color_channels;
            input_row[input..input + input_color_channels]
                .copy_from_slice(&color_samples[source..source + input_color_channels]);
        }

        let colors = if let Some(transform) = &mut transforms {
            transform
                .do_transform(&input_row, &mut output_row)
                .map_err(|error| error.to_string())?;
            &output_row[..]
        } else {
            &input_row[..]
        };

        for x in 0..info.size.0 {
            let pixel = y * info.size.0 + x;
            let color = x * if transforms.is_some() {
                3
            } else {
                input_color_channels
            };
            let destination = pixel * 4;
            if grayscale {
                let value = to_byte(colors[color]);
                rgba[destination..destination + 3].fill(value);
            } else {
                rgba[destination] = to_byte(colors[color]);
                rgba[destination + 1] = to_byte(colors[color + 1]);
                rgba[destination + 2] = to_byte(colors[color + 2]);
            }

            let alpha = if cmyk {
                separate_alpha.map_or(1.0, |_| alpha_samples.as_ref().unwrap()[pixel])
            } else if alpha_index.is_some() {
                color_samples[pixel * channels + channels - 1]
            } else {
                1.0
            };
            rgba[destination + 3] = to_byte(alpha);
        }
    }
    Ok((width, height, rgba))
}

impl ImageHandler {
    fn parse(&self, bytes: Bytes) -> Result<Resource, String> {
        #[cfg(feature = "image-formats")]
        if is_jxl(&bytes) {
            return decode_jxl(&bytes)
                .map(|(width, height, rgba)| {
                    Resource::Image(self.kind, width, height, Arc::new(rgba))
                })
                .map_err(|error| {
                    format!(
                        "Could not parse JPEG XL image ({} bytes): {error}",
                        bytes.len()
                    )
                });
        }

        let image_err = match image::ImageReader::new(Cursor::new(&bytes))
            .with_guessed_format()
            .expect("IO errors impossible with Cursor")
            .decode()
        {
            Ok(image) => {
                let raw_rgba8_data = image.clone().into_rgba8().into_raw();
                return Ok(Resource::Image(
                    self.kind,
                    image.width(),
                    image.height(),
                    Arc::new(raw_rgba8_data),
                ));
            }
            Err(e) => e.to_string(),
        };

        #[cfg(feature = "svg")]
        let svg_err = {
            use crate::util::parse_svg_image;
            match parse_svg_image(&bytes) {
                Ok(svg) => return Ok(Resource::Svg(self.kind, svg)),
                Err(e) => e.to_string(),
            }
        };
        #[cfg(not(feature = "svg"))]
        let svg_err = "svg feature disabled";

        Err(format!(
            "Could not parse image ({} bytes): image-crate error: {image_err}; svg fallback error: {svg_err}",
            bytes.len()
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use parley::fontique::FontStyle as Fq;
    use style::values::specified::Angle;

    fn oblique(min_deg: f32, max_deg: f32) -> FontStyleRange {
        FontStyleRange::Oblique(Angle::from_degrees(min_deg), Angle::from_degrees(max_deg))
    }

    #[test]
    fn italic_maps_to_italic() {
        assert_eq!(stylo_to_fontique_style(&FontStyleRange::Italic), Fq::Italic,);
    }

    #[test]
    fn oblique_zero_zero_maps_to_normal() {
        // Stylo parses bare CSS `normal` as `Oblique(0deg, 0deg)`; the
        // helper must round-trip that back to `FontStyle::Normal` so
        // parley's matching doesn't misclassify upright fonts.
        assert_eq!(stylo_to_fontique_style(&oblique(0.0, 0.0)), Fq::Normal);
    }

    #[test]
    fn oblique_single_angle_maps_to_oblique_with_min() {
        assert_eq!(
            stylo_to_fontique_style(&oblique(14.0, 14.0)),
            Fq::Oblique(Some(14.0)),
        );
    }

    #[test]
    fn oblique_range_uses_min_angle() {
        // For a range, fontique's single-angle representation takes the
        // lower bound — confirm `min` (not `max`) is what gets through.
        assert_eq!(
            stylo_to_fontique_style(&oblique(10.0, 20.0)),
            Fq::Oblique(Some(10.0)),
        );
    }
}
