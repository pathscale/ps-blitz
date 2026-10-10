//! CSSOM operations that use the same parser and computed values as rendering.

use cssparser::{Parser, ParserInput, serialize_identifier};
use selectors::matching::QuirksMode;
use style::media_queries::MediaList;
use style::parser::ParserContext;
use style::properties::{
    ComputedValues, Importance, IndexedId, LonghandId, PropertyDeclaration,
    PropertyDeclarationBlock, PropertyDeclarationId, PropertyId, SourcePropertyDeclaration,
    parse_style_attribute,
};
use style::selector_parser::PseudoElement;
use style::servo_arc::Arc as ServoArc;
use style::shared_lock::ToCssWithGuard;
use style::stylesheets::{
    CssRuleType, CustomMediaEvaluator, DocumentStyleSheet, Origin, OriginSet,
    StylesheetInDocument,
};
use style::stylesheets::supports_rule::{Declaration, SupportsCondition};
use style_traits::{CssString, ParsingMode, ToCss};

use crate::{BaseDocument, NodeId, local_name};

/// A parsed media query owned by its MediaQueryList, rather than a parser cache.
pub struct PlatformMediaQuery {
    list: MediaList,
}

impl PlatformMediaQuery {
    /// The CSSOM serialization of this query.
    pub fn media(&self) -> String {
        self.list.to_css_string()
    }
}

/// Keeps a sheet alive if its owner is removed from the document.
pub struct PlatformStyleSheet {
    sheet: DocumentStyleSheet,
    href: Option<String>,
    origin_clean: bool,
}

impl PlatformStyleSheet {
    /// The fetched sheet's URL, or None for an inline sheet.
    pub fn href(&self) -> Option<&str> {
        self.href.as_deref()
    }

    /// Whether CSSOM has disabled the sheet.
    pub fn disabled(&self) -> bool {
        self.sheet.0.disabled()
    }

    /// The sheet's current media text.
    pub fn media(&self) -> String {
        let guard = self.sheet.0.shared_lock.read();
        self.sheet.0.media.read_with(&guard).to_css_string()
    }

    /// Individually serialized media queries.
    pub fn media_items(&self) -> Vec<String> {
        let guard = self.sheet.0.shared_lock.read();
        self.sheet
            .0
            .media
            .read_with(&guard)
            .media_queries
            .iter()
            .map(ToCss::to_css_string)
            .collect()
    }

    /// Serialize available rules. Cross-origin sheets require an origin-clean flag.
    pub fn rules(&self) -> Option<Vec<String>> {
        if !self.origin_clean {
            return None;
        }
        let guard = self.sheet.0.shared_lock.read();
        Some(
            self.sheet
                .contents(&guard)
                .rules(&guard)
                .iter()
                .map(|rule| {
                    let mut text = CssString::new();
                    rule.to_css(&guard, &mut text)
                        .expect("writing CSS to a string cannot fail");
                    text.to_string()
                })
                .collect(),
        )
    }
}

impl BaseDocument {
    fn platform_parser_context<'a>(
        &'a self,
        url_data: &'a style::stylesheets::UrlExtraData,
    ) -> ParserContext<'a> {
        ParserContext::new(
            Origin::Author,
            url_data,
            Some(CssRuleType::Style),
            ParsingMode::DEFAULT,
            QuirksMode::NoQuirks,
            Default::default(),
            None,
            None,
            Default::default(),
        )
    }

    /// Test a supports condition through Stylo.
    pub fn platform_supports(&self, condition: &str) -> bool {
        let url_data = self.url.url_extra_data();
        let mut context = self.platform_parser_context(&url_data);
        let mut input = ParserInput::new(condition);
        if let Ok(condition) =
            Parser::new(&mut input).parse_entirely(SupportsCondition::parse)
        {
            return condition.eval(&mut context);
        }
        // CSS.supports also accepts a declaration with implicit parentheses.
        let mut input = ParserInput::new(condition);
        Parser::new(&mut input)
            .parse_entirely(Declaration::parse)
            .is_ok_and(|declaration| declaration.eval(&mut context))
    }

    /// Test a property and value without allowing the property string to inject syntax.
    pub fn platform_supports_property(&self, name: &str, value: &str) -> bool {
        let mut declaration = String::new();
        serialize_identifier(name, &mut declaration)
            .expect("writing an identifier to a string cannot fail");
        declaration.push(':');
        declaration.push_str(value);
        self.platform_supports(&declaration)
    }

    /// Parse a query with the document's CSS parser context.
    pub fn platform_media_query(&self, text: &str) -> PlatformMediaQuery {
        let url_data = self.url.url_extra_data();
        let mut context = self.platform_parser_context(&url_data);
        let mut input = ParserInput::new(text);
        PlatformMediaQuery {
            list: MediaList::parse(&mut context, &mut Parser::new(&mut input)),
        }
    }

    /// Evaluate against the current Stylo device, including viewport and color scheme.
    pub fn platform_media_matches(&self, query: &PlatformMediaQuery) -> bool {
        query.list.evaluate(
            self.stylist.device(),
            QuirksMode::NoQuirks,
            &mut CustomMediaEvaluator::none(),
        )
    }

    fn platform_computed_values(
        &self,
        node_id: NodeId,
        pseudo: &str,
    ) -> Option<ServoArc<ComputedValues>> {
        let node = self.get_node(node_id)?;
        if !node.is_element() || !node.flags.is_in_document() {
            return None;
        }
        if pseudo.is_empty() {
            return node.primary_styles().map(|style| ServoArc::clone(&style));
        }
        let pseudo = match pseudo {
            "::before" => PseudoElement::Before,
            "::after" => PseudoElement::After,
            _ => return None,
        };
        let data = node.stylo_element_data_opt()?.get()?;
        data.styles.pseudos.get(&pseudo).cloned()
    }

    /// Canonical, enabled longhands followed by valid custom properties.
    pub fn platform_computed_names(&self, node_id: NodeId, pseudo: &str) -> Vec<String> {
        let Some(style) = self.platform_computed_values(node_id, pseudo) else {
            return Vec::new();
        };
        let mut names = Vec::new();
        for index in 0..LonghandId::COUNT {
            // IndexedId guarantees contiguous valid ids below COUNT.
            let id = unsafe { LonghandId::from_index_release_unchecked(index) };
            if PropertyId::parse_enabled_for_all_content(id.name()).is_ok() {
                names.push(id.name().to_string());
            }
        }
        names.sort_unstable();
        let properties = style.custom_properties();
        let count = properties.inherited.len() + properties.non_inherited.len();
        for index in 0..count {
            if let Some((name, Some(_))) = properties.property_at(index) {
                names.push(format!("--{name}"));
            }
        }
        names
    }

    /// Serialize any enabled longhand or custom property from computed values.
    pub fn platform_computed_value(
        &self,
        node_id: NodeId,
        pseudo: &str,
        name: &str,
    ) -> String {
        let Some(style) = self.platform_computed_values(node_id, pseudo) else {
            return String::new();
        };
        let Ok(property) = PropertyId::parse_enabled_for_all_content(name) else {
            return String::new();
        };
        let declaration = match property.as_shorthand() {
            Ok(_) => return String::new(),
            Err(declaration) => declaration,
        };

        // Preserve the renderer's existing used box values for geometry.
        let geometry_id = self.get_node(node_id).and_then(|node| match pseudo {
            "" => Some(node_id),
            "::before" => node.before(),
            "::after" => node.after(),
            _ => None,
        });
        if matches!(
            name,
            "width"
                | "height"
                | "padding-top"
                | "padding-right"
                | "padding-bottom"
                | "padding-left"
                | "margin-top"
                | "margin-right"
                | "margin-bottom"
                | "margin-left"
                | "border-top-width"
                | "border-right-width"
                | "border-bottom-width"
                | "border-left-width"
        ) {
            if let Some(properties) = geometry_id
                .and_then(|id| self.get_node(id))
                .and_then(|node| node.computed_style_properties())
            {
                if let Some((_, value)) = properties.into_iter().find(|(key, _)| *key == name) {
                    return value;
                }
            }
        }
        style.computed_value_to_string(declaration)
    }

    fn platform_inline_block(&self, node_id: NodeId) -> PropertyDeclarationBlock {
        let css = self
            .get_node(node_id)
            .and_then(|node| node.attr(local_name!("style")))
            .unwrap_or_default();
        parse_style_attribute(
            css,
            &self.url.url_extra_data(),
            None,
            QuirksMode::NoQuirks,
            CssRuleType::Style,
        )
    }

    /// Parse and serialize cssText without splitting CSS on punctuation.
    pub fn platform_inline_text(&self, node_id: NodeId) -> String {
        let mut text = CssString::new();
        self.platform_inline_block(node_id)
            .to_css(&mut text)
            .expect("writing CSS to a string cannot fail");
        text.to_string()
    }

    /// The declaration's ordered longhand and custom-property names.
    pub fn platform_inline_names(&self, node_id: NodeId) -> Vec<String> {
        self.platform_inline_block(node_id)
            .declarations()
            .iter()
            .map(|declaration| match declaration.id() {
                PropertyDeclarationId::Longhand(id) => id.name().to_string(),
                PropertyDeclarationId::Custom(name) => format!("--{name}"),
            })
            .collect()
    }

    /// Read a value or priority from Stylo's parsed declaration block.
    pub fn platform_inline_value(&self, node_id: NodeId, name: &str, priority: bool) -> String {
        let url_data = self.url.url_extra_data();
        let context = self.platform_parser_context(&url_data);
        let Ok(property) = PropertyId::parse(name, &context) else {
            return String::new();
        };
        let block = self.platform_inline_block(node_id);
        if priority {
            return if block.property_priority(&property).important() {
                "important".to_string()
            } else {
                String::new()
            };
        }
        let mut text = CssString::new();
        block
            .property_value_to_css(&property, &mut text)
            .expect("writing CSS to a string cannot fail");
        text.to_string()
    }

    /// Return replacement cssText for a CSSOM edit, or None for an invalid edit.
    ///
    /// The binding applies the returned text through the ordinary attribute mutator.
    pub fn platform_edit_inline(
        &self,
        node_id: NodeId,
        name: &str,
        value: &str,
        priority: &str,
    ) -> Option<String> {
        let url_data = self.url.url_extra_data();
        let context = self.platform_parser_context(&url_data);
        let property = PropertyId::parse(name, &context).ok()?;
        let mut source = SourcePropertyDeclaration::default();
        let importance = if priority.is_empty() {
            Importance::Normal
        } else if priority.eq_ignore_ascii_case("important") {
            Importance::Important
        } else if !value.is_empty() {
            return None;
        } else {
            Importance::Normal
        };
        if !value.is_empty() {
            let mut input = ParserInput::new(value);
            Parser::new(&mut input)
                .parse_entirely(|input| {
                    PropertyDeclaration::parse_into(
                        &mut source,
                        property.clone(),
                        &context,
                        input,
                    )
                })
                .ok()?;
        }
        let mut block = self.platform_inline_block(node_id);
        if let Some(index) = block.first_declaration_to_remove(&property) {
            block.remove_property(&property, index);
        }
        if !value.is_empty() {
            block.extend(source.drain(), importance);
        }
        let mut text = CssString::new();
        block
            .to_css(&mut text)
            .expect("writing CSS to a string cannot fail");
        Some(text.to_string())
    }

    /// Sheet owners in DOM order, scoped to the requested document.
    pub fn platform_sheet_owners(&self, root_id: NodeId) -> Vec<NodeId> {
        let mut owners = Vec::new();
        let mut stack = vec![root_id];
        while let Some(id) = stack.pop() {
            let Some(node) = self.get_node(id) else {
                continue;
            };
            if self.nodes_to_stylesheet.contains_key(&id) {
                owners.push(id);
            }
            stack.extend(node.children.iter().rev().copied());
        }
        owners
    }

    /// Retain the actual sheet, including its final fetched URL.
    pub fn platform_sheet(&self, owner: NodeId) -> Option<PlatformStyleSheet> {
        let sheet = self.nodes_to_stylesheet.get(&owner)?.clone();
        let node = self.get_node(owner)?;
        let guard = sheet.0.shared_lock.read();
        let source_url = &sheet.contents(&guard).url_data.0;
        let linked = node
            .element_data()
            .is_some_and(|element| element.name.local == local_name!("link"));
        let href = linked.then(|| source_url.to_string());
        let origin_clean = !linked || source_url.origin() == self.url.origin();
        drop(guard);
        Some(PlatformStyleSheet {
            sheet,
            href,
            origin_clean,
        })
    }

    /// Apply an owner's initial media attribute to its sheet.
    pub(crate) fn platform_initial_sheet_media(
        &self,
        sheet: &DocumentStyleSheet,
        owner: NodeId,
    ) {
        let text = self
            .get_node(owner)
            .and_then(|node| node.attr(local_name!("media")))
            .unwrap_or_default();
        let query = self.platform_media_query(text);
        *sheet.0.media.write_with(&mut sheet.0.shared_lock.write()) = query.list;
    }

    /// Disable a sheet and invalidate the cascade.
    pub fn platform_disable_sheet(&mut self, sheet: &PlatformStyleSheet, disabled: bool) {
        if sheet.sheet.0.set_disabled(disabled) {
            self.stylist
                .force_stylesheet_origins_dirty(OriginSet::all());
        }
    }

    /// Replace a sheet's media list and invalidate the cascade.
    pub fn platform_set_sheet_media(&mut self, sheet: &PlatformStyleSheet, text: &str) {
        let query = self.platform_media_query(text);
        *sheet
            .sheet
            .0
            .media
            .write_with(&mut sheet.sheet.0.shared_lock.write()) = query.list;
        self.stylist
            .force_stylesheet_origins_dirty(OriginSet::all());
    }

    /// Append or delete a parsed medium. False means invalid or not found.
    pub fn platform_edit_sheet_medium(
        &mut self,
        sheet: &PlatformStyleSheet,
        medium: &str,
        delete: bool,
    ) -> bool {
        let url_data = self.url.url_extra_data();
        let context = self.platform_parser_context(&url_data);
        let changed = {
            let mut guard = sheet.sheet.0.shared_lock.write();
            let list = sheet.sheet.0.media.write_with(&mut guard);
            if delete {
                list.delete_medium(&context, medium)
            } else {
                list.append_medium(&context, medium)
            }
        };
        if changed {
            self.stylist
                .force_stylesheet_origins_dirty(OriginSet::all());
        }
        changed
    }

    /// Return all hit elements in front-to-back paint order.
    pub fn platform_elements_from_point(&self, x: f32, y: f32) -> Vec<NodeId> {
        let viewport = self.get_viewport();
        let width = viewport.window_size.0 as f64 / viewport.scale_f64();
        let height = viewport.window_size.1 as f64 / viewport.scale_f64();
        if !x.is_finite()
            || !y.is_finite()
            || x < 0.0
            || y < 0.0
            || f64::from(x) >= width
            || f64::from(y) >= height
        {
            return Vec::new();
        }
        let Some(root) = self.try_root_element() else {
            return Vec::new();
        };
        let mut hits = Some(Vec::new());
        root.hit_inner_collect(
            x,
            y,
            viewport.scale_f64(),
            &mut None,
            &mut hits,
        );
        let mut seen = std::collections::HashSet::new();
        hits.unwrap_or_default()
            .into_iter()
            .filter_map(|hit| {
                let mut id = self.nearest_non_anonymous_ancestor(hit.node_id)?;
                loop {
                    let node = self.get_node(id)?;
                    if node.is_element() {
                        return seen.insert(id).then_some(id);
                    }
                    id = node.parent?;
                }
            })
            .collect()
    }
}
