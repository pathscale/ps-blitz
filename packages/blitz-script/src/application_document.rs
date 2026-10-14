use std::sync::Arc;
use std::task::Context;

use blitz_dom::{DocGuard, DocGuardMut, Document, DocumentConfig};
use blitz_traits::events::UiEvent;

#[cfg(feature = "boa")]
use crate::ScriptDocument;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum ScriptBackend {
    #[default]
    Boa,
    Custom,
}

/// A document backend selected before bootstrap or host installation.
pub trait ScriptDocumentBackend: Document + Sized {
    fn from_html(html: Arc<str>, config: DocumentConfig) -> Result<Self, String>;
    fn execute_scripts(&mut self) -> Result<(), String>;
}

#[cfg(feature = "boa")]
impl ScriptDocumentBackend for ScriptDocument {
    fn from_html(html: Arc<str>, config: DocumentConfig) -> Result<Self, String> {
        Ok(Self::from_html(&html, config))
    }

    fn execute_scripts(&mut self) -> Result<(), String> {
        Self::execute_scripts(self);
        Ok(())
    }
}

/// Selection happens before either backend's bootstrap or host installers.
#[cfg(feature = "boa")]
pub enum ApplicationDocument<D: ScriptDocumentBackend = ScriptDocument> {
    Boa(Box<ScriptDocument>),
    Custom(Box<D>),
}

/// Selection happens before the custom backend's bootstrap or host installers.
#[cfg(not(feature = "boa"))]
pub enum ApplicationDocument<D: ScriptDocumentBackend> {
    Custom(Box<D>),
}

impl<D: ScriptDocumentBackend> ApplicationDocument<D> {
    pub fn from_html(
        html: Arc<str>,
        config: DocumentConfig,
        backend: ScriptBackend,
    ) -> Result<Self, String> {
        match backend {
            #[cfg(feature = "boa")]
            ScriptBackend::Boa => Ok(Self::Boa(Box::new(ScriptDocument::from_html(
                &html, config,
            )))),
            #[cfg(not(feature = "boa"))]
            ScriptBackend::Boa => Err("the Boa backend requires the `boa` feature".into()),
            ScriptBackend::Custom => {
                D::from_html(html, config).map(|document| Self::Custom(Box::new(document)))
            }
        }
    }

    pub fn backend(&self) -> ScriptBackend {
        match self {
            #[cfg(feature = "boa")]
            Self::Boa(_) => ScriptBackend::Boa,
            Self::Custom(_) => ScriptBackend::Custom,
        }
    }

    pub fn execute_scripts(&mut self) -> Result<(), String> {
        match self {
            #[cfg(feature = "boa")]
            Self::Boa(document) => {
                document.execute_scripts();
                Ok(())
            }
            Self::Custom(document) => document.execute_scripts(),
        }
    }
}

impl<D: ScriptDocumentBackend> Document for ApplicationDocument<D> {
    fn inner(&self) -> DocGuard<'_> {
        match self {
            #[cfg(feature = "boa")]
            Self::Boa(document) => document.inner(),
            Self::Custom(document) => document.inner(),
        }
    }
    fn inner_mut(&mut self) -> DocGuardMut<'_> {
        match self {
            #[cfg(feature = "boa")]
            Self::Boa(document) => document.inner_mut(),
            Self::Custom(document) => document.inner_mut(),
        }
    }
    fn handle_ui_event(&mut self, event: UiEvent) {
        match self {
            #[cfg(feature = "boa")]
            Self::Boa(document) => document.handle_ui_event(event),
            Self::Custom(document) => document.handle_ui_event(event),
        }
    }
    fn poll(&mut self, context: Option<Context>) -> bool {
        match self {
            #[cfg(feature = "boa")]
            Self::Boa(document) => document.poll(context),
            Self::Custom(document) => document.poll(context),
        }
    }
    fn poll_for_settle(&mut self, context: Option<Context>, timers: bool) -> bool {
        match self {
            #[cfg(feature = "boa")]
            Self::Boa(document) => document.poll_for_settle(context, timers),
            Self::Custom(document) => document.poll_for_settle(context, timers),
        }
    }
}

