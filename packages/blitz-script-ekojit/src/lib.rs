//! Owner-local native page execution for Blitz through ekojit.

mod native;
mod native_document;

pub use ekojit::page::{NativeReceipt, PageError, PageSource};
pub use native::NativePageDefect;
pub use native_document::EkojitDocument;
