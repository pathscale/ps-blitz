//! Font loading records shared by CSS, script and resource delivery.

use std::fmt;
use std::sync::Arc;
use std::sync::atomic::{AtomicU8, AtomicUsize, Ordering};

use blitz_traits::net::Bytes;
use blitz_traits::shell::ShellProvider;
use cssparser::{Parser, ParserInput, Token, serialize_string};
use style::font_face::Source;
use style::parser::ParserContext;
use style::properties::{PropertyDeclaration, PropertyId, SourcePropertyDeclaration};
use style::stylesheets::{CssRule, CssRuleType, Origin, StylesheetInDocument};
use style_traits::{ParsingMode, ToCss};
use url::Url;

use crate::BaseDocument;

use super::FontFaceOverrides;

pub struct WebFont {
    pub id: usize,
    pub(crate) overrides: FontFaceOverrides,
    pub(crate) source: Option<Url>,
    pub(crate) data: Option<Bytes>,
    pub(crate) css: bool,
    pub(crate) epoch: usize,
    status: AtomicU8,
    shell: Arc<dyn ShellProvider>,
}

impl fmt::Debug for WebFont {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("WebFont")
            .field("id", &self.id)
            .field("family", &self.family())
            .field("status", &self.status())
            .finish()
    }
}

impl WebFont {
    pub(crate) fn new(
        overrides: FontFaceOverrides,
        source: Option<Url>,
        data: Option<Bytes>,
        css: bool,
        epoch: usize,
        shell: Arc<dyn ShellProvider>,
    ) -> Arc<Self> {
        static NEXT_ID: AtomicUsize = AtomicUsize::new(1);
        Arc::new(Self {
            id: NEXT_ID.fetch_add(1, Ordering::Relaxed),
            overrides,
            source,
            data,
            css,
            epoch,
            status: AtomicU8::new(if css { 1 } else { 0 }),
            shell,
        })
    }

    pub fn family(&self) -> &str {
        self.overrides.family_name.as_deref().unwrap_or("")
    }

    pub fn style(&self) -> &'static str {
        use parley::fontique::FontStyle;
        match self.overrides.style {
            Some(FontStyle::Italic) => "italic",
            Some(FontStyle::Oblique(_)) => "oblique",
            _ => "normal",
        }
    }

    pub fn weight(&self) -> f32 {
        self.overrides.weight.unwrap_or(400.0)
    }

    pub fn status(&self) -> &'static str {
        match self.status.load(Ordering::Acquire) {
            0 => "unloaded",
            1 => "loading",
            2 => "loaded",
            _ => "error",
        }
    }

    pub(crate) fn start(&self) -> bool {
        self.status
            .compare_exchange(0, 1, Ordering::AcqRel, Ordering::Acquire)
            .is_ok()
    }

    pub(crate) fn finish(&self, success: bool) {
        self.status
            .store(if success { 2 } else { 3 }, Ordering::Release);
        self.shell.request_redraw();
    }
}

impl BaseDocument {
    pub fn web_fonts(&self) -> &[Arc<WebFont>] {
        &self.web_fonts
    }

    pub fn web_fonts_pending(&self) -> bool {
        self.has_pending_critical_resources()
            || self.web_fonts.iter().any(|face| face.status() == "loading")
    }

    pub(crate) fn reset_web_fonts(&mut self) {
        self.font_epoch = self.font_epoch.wrapping_add(1);
        for face in self.web_fonts.iter().filter(|face| face.css) {
            if face.status() == "loading" {
                face.finish(false);
            }
        }
        self.web_fonts.retain(|face| !face.css);
    }

    pub fn make_web_font(
        &self,
        family: &str,
        source: Option<&str>,
        bytes: Option<Bytes>,
        font_style: &str,
        weight: &str,
    ) -> Result<Arc<WebFont>, String> {
        if family.is_empty() {
            return Err(String::from("Empty font family"));
        }
        let mut quoted = String::new();
        serialize_string(family, &mut quoted).expect("writing CSS cannot fail");
        let css = format!(
            "@font-face {{ font-family: {quoted}; src: {}; font-style: {font_style}; font-weight: {weight}; }}",
            source.unwrap_or("url('about:blank')")
        );
        let sheet = self.make_stylesheet(css, Origin::Author);
        let guard = self.guard.read();
        let face = sheet
            .contents(&guard)
            .rules(&guard)
            .iter()
            .find_map(|rule| match rule {
                CssRule::FontFace(face) => Some(face),
                _ => None,
            })
            .ok_or_else(|| String::from("Invalid font descriptors"))?;
        let descriptors = &face.read_with(&guard).descriptors;
        let family = descriptors
            .font_family
            .as_ref()
            .ok_or_else(|| String::from("Invalid font family"))?;
        let style = descriptors
            .font_style
            .as_ref()
            .ok_or_else(|| String::from("Invalid font style"))?;
        let weight = descriptors
            .font_weight
            .as_ref()
            .and_then(|weight| weight.0.compute())
            .ok_or_else(|| String::from("Invalid font weight"))?;
        let parsed_source = descriptors
            .src
            .as_ref()
            .ok_or_else(|| String::from("Invalid font source"))?;
        let url = if bytes.is_some() {
            None
        } else {
            Some(
                parsed_source
                    .0
                    .iter()
                    .find_map(|source| match source {
                        Source::Url(source) => source.url.url().map(|url| url.as_ref().clone()),
                        Source::Local(_) => None,
                    })
                    .ok_or_else(|| String::from("A URL font source is required"))?,
            )
        };
        Ok(WebFont::new(
            FontFaceOverrides {
                family_name: Some(family.name.to_string()),
                weight: Some(weight.value()),
                style: Some(super::stylo_to_fontique_style(style)),
            },
            url,
            bytes,
            false,
            self.font_epoch,
            self.shell_provider.clone(),
        ))
    }

    pub fn load_web_font(&mut self, face: &Arc<WebFont>) {
        super::load_script_font(self, face);
    }

    pub fn set_web_font_member(&mut self, face: &Arc<WebFont>, member: bool) -> bool {
        let index = self.web_fonts.iter().position(|item| item.id == face.id);
        if member {
            if index.is_none() {
                self.web_fonts.push(Arc::clone(face));
            }
            true
        } else if face.css {
            false
        } else if let Some(index) = index {
            self.web_fonts.remove(index);
            self.invalidate_inline_contexts();
            true
        } else {
            false
        }
    }

    pub fn matching_web_fonts(&self, shorthand: &str) -> Result<Vec<Arc<WebFont>>, String> {
        let url_data = self.url.url_extra_data();
        let context = ParserContext::new(
            Origin::Author,
            &url_data,
            Some(CssRuleType::Style),
            ParsingMode::DEFAULT,
            selectors::matching::QuirksMode::NoQuirks,
            Default::default(),
            None,
            None,
            Default::default(),
        );
        let property = PropertyId::parse_enabled_for_all_content("font")
            .map_err(|_| String::from("Font shorthand is unavailable"))?;
        let mut declarations = SourcePropertyDeclaration::default();
        let mut input = ParserInput::new(shorthand);
        Parser::new(&mut input)
            .parse_entirely(|parser| {
                PropertyDeclaration::parse_into(&mut declarations, property, &context, parser)
            })
            .map_err(|_| String::from("Invalid font shorthand"))?;

        let mut families = String::new();
        let mut font_style = String::from("normal");
        let mut weight = 400.0_f32;
        for declaration in declarations.declarations.drain(..) {
            match declaration {
                PropertyDeclaration::FontFamily(value) => families = value.to_css_string(),
                PropertyDeclaration::FontStyle(value) => font_style = value.to_css_string(),
                PropertyDeclaration::FontWeight(value) => {
                    let text = value.to_css_string();
                    weight = match text.as_str() {
                        "normal" => 400.0,
                        "bold" => 700.0,
                        _ => text.parse().unwrap_or(400.0),
                    };
                }
                _ => {}
            }
        }
        if families.is_empty() {
            return Err(String::from("A font family is required"));
        }
        let mut input = ParserInput::new(&families);
        let mut parser = Parser::new(&mut input);
        let mut names = Vec::new();
        let mut name = String::new();
        while !parser.is_exhausted() {
            match parser
                .next()
                .map_err(|_| String::from("Invalid font family"))?
            {
                Token::Ident(value) | Token::QuotedString(value) => {
                    if !name.is_empty() {
                        name.push(' ');
                    }
                    name.push_str(value);
                }
                Token::Comma => {
                    names.push(std::mem::take(&mut name));
                }
                _ => return Err(String::from("Invalid font family")),
            }
        }
        names.push(name);
        let style = if font_style.starts_with("italic") {
            "italic"
        } else if font_style.starts_with("oblique") {
            "oblique"
        } else {
            "normal"
        };
        let mut matched = Vec::new();
        for name in names {
            let family: Vec<_> = self
                .web_fonts
                .iter()
                .filter(|face| face.family().eq_ignore_ascii_case(&name))
                .collect();
            let has_style = family.iter().any(|face| face.style() == style);
            let distance = family
                .iter()
                .filter(|face| !has_style || face.style() == style)
                .map(|face| (face.weight() - weight).abs())
                .min_by(f32::total_cmp);
            if let Some(distance) = distance {
                for face in family {
                    if (!has_style || face.style() == style)
                        && (face.weight() - weight).abs() == distance
                        && !matched.iter().any(|item: &Arc<WebFont>| item.id == face.id)
                    {
                        matched.push(Arc::clone(face));
                    }
                }
            }
        }
        Ok(matched)
    }
}
