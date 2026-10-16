use blitz_dom::node::NodeFlags;
use blitz_dom::{BaseDocument, LocalName};

/// A script found in the parsed document, in tree order.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct DocumentScript {
    /// Inline source, or an empty string when the script has an external source.
    pub source: String,
    /// The value of the `src` attribute, when present.
    pub src: Option<String>,
    /// How the script should be evaluated.
    pub kind: ScriptKind,
}

/// The supported executable script forms.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScriptKind {
    Classic,
    Module,
    ImportMap,
}

/// Collect executable scripts from the document in DOM tree order.
pub(crate) fn document_scripts(doc: &BaseDocument) -> Vec<DocumentScript> {
    let mut scripts = Vec::new();
    let mut pending = doc
        .root_node()
        .children
        .iter()
        .rev()
        .copied()
        .collect::<Vec<_>>();

    while let Some(node_id) = pending.pop() {
        let Some(node) = doc.get_node(node_id) else {
            continue;
        };

        if let Some(element) = node.element_data() {
            let is_html = element.name.ns.as_ref() == "http://www.w3.org/1999/xhtml";
            let is_template = is_html && element.name.local.as_ref() == "template";
            if is_template {
                continue;
            }

            if is_html && element.name.local.as_ref() == "script" {
                if node.flags.contains(NodeFlags::IS_PARSER_INERT_SCRIPT) {
                    continue;
                }

                if let Some(kind) = script_kind(element) {
                    let src = element.attr(LocalName::from("src")).map(ToOwned::to_owned);
                    let source = if src.is_some() {
                        String::new()
                    } else {
                        let mut source = String::new();
                        node.write_text_content(&mut source);
                        source
                    };
                    scripts.push(DocumentScript { source, src, kind });
                }
            }
        }

        pending.extend(node.children.iter().rev().copied());
    }

    scripts
}

fn script_kind(element: &blitz_dom::node::ElementData) -> Option<ScriptKind> {
    let type_value = element.attr(LocalName::from("type"));
    let kind = match type_value.map(trim_ascii_whitespace) {
        Some(value) if value.eq_ignore_ascii_case("module") => ScriptKind::Module,
        Some(value) if value.eq_ignore_ascii_case("importmap") => ScriptKind::ImportMap,
        Some("") | None => {
            let language = element.attr(LocalName::from("language"));
            match language.map(trim_ascii_whitespace) {
                Some(language) if !language.is_empty() => {
                    let mut mime = String::from("text/");
                    mime.push_str(language);
                    if !is_javascript_mime_type(&mime) {
                        return None;
                    }
                    ScriptKind::Classic
                }
                _ => ScriptKind::Classic,
            }
        }
        Some(value) if is_javascript_mime_type(value) => ScriptKind::Classic,
        Some(_) => return None,
    };

    if kind == ScriptKind::Classic && element.has_attr(LocalName::from("nomodule")) {
        return None;
    }

    Some(kind)
}

fn is_javascript_mime_type(value: &str) -> bool {
    let essence =
        trim_ascii_whitespace(value.split_once(';').map_or(value, |(essence, _)| essence));

    matches_ignore_ascii_case(
        essence,
        &[
            "application/ecmascript",
            "application/javascript",
            "application/x-ecmascript",
            "application/x-javascript",
            "text/ecmascript",
            "text/javascript",
            "text/javascript1.0",
            "text/javascript1.1",
            "text/javascript1.2",
            "text/javascript1.3",
            "text/javascript1.4",
            "text/javascript1.5",
            "text/jscript",
            "text/livescript",
            "text/x-ecmascript",
            "text/x-javascript",
        ],
    )
}

fn matches_ignore_ascii_case(value: &str, choices: &[&str]) -> bool {
    choices
        .iter()
        .any(|choice| value.eq_ignore_ascii_case(choice))
}

fn trim_ascii_whitespace(value: &str) -> &str {
    value.trim_matches(|character: char| character.is_ascii_whitespace())
}
