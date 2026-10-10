#![allow(clippy::collapsible_if)]

mod html_document;
mod html_sink;
mod preload;
pub mod stream;

pub use html_document::HtmlDocument;
pub use html_sink::DocumentHtmlParser;
pub use html_sink::HtmlProvider;
pub use preload::{ScriptTag, scan_script_tags};
