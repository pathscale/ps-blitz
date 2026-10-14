use std::sync::Arc;
use std::task::Context;

use blitz_dom::{DocGuard, DocGuardMut, Document, DocumentConfig};
use blitz_traits::events::UiEvent;

use crate::ScriptDocument;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum ScriptBackend {
    #[default]
    Boa,
    Ekojit,
}

/// Selection happens before either backend's bootstrap or host installers.
pub enum ApplicationDocument {
    Boa(ScriptDocument),
    #[cfg(feature = "ekojit")]
    Ekojit(crate::EkojitDocument),
}

impl ApplicationDocument {
    pub fn from_html(html: Arc<str>, config: DocumentConfig, backend: ScriptBackend) -> Result<Self, String> {
        match backend {
            ScriptBackend::Boa => Ok(Self::Boa(ScriptDocument::from_html(&html, config))),
            ScriptBackend::Ekojit => {
                #[cfg(feature = "ekojit")]
                { crate::EkojitDocument::from_html(html, config, crate::NativePageDefect::None)
                    .map(Self::Ekojit).map_err(|error| error.to_string()) }
                #[cfg(not(feature = "ekojit"))]
                { Err("ekojit requires the blitz-script ekojit cargo feature".into()) }
            }
        }
    }

    pub fn backend(&self) -> ScriptBackend {
        match self {
            Self::Boa(_) => ScriptBackend::Boa,
            #[cfg(feature = "ekojit")]
            Self::Ekojit(_) => ScriptBackend::Ekojit,
        }
    }

    pub fn execute_scripts(&mut self) -> Result<(), String> {
        match self {
            Self::Boa(document) => { document.execute_scripts(); Ok(()) }
            #[cfg(feature = "ekojit")]
            Self::Ekojit(document) => document.execute_scripts().map_err(|error| error.to_string()),
        }
    }
}

impl Document for ApplicationDocument {
    fn inner(&self) -> DocGuard<'_> {
        match self { Self::Boa(document) => document.inner(),
            #[cfg(feature = "ekojit")] Self::Ekojit(document) => document.inner() }
    }
    fn inner_mut(&mut self) -> DocGuardMut<'_> {
        match self { Self::Boa(document) => document.inner_mut(),
            #[cfg(feature = "ekojit")] Self::Ekojit(document) => document.inner_mut() }
    }
    fn handle_ui_event(&mut self, event: UiEvent) {
        match self { Self::Boa(document) => document.handle_ui_event(event),
            #[cfg(feature = "ekojit")] Self::Ekojit(document) => document.handle_ui_event(event) }
    }
    fn poll(&mut self, context: Option<Context>) -> bool {
        match self { Self::Boa(document) => document.poll(context),
            #[cfg(feature = "ekojit")] Self::Ekojit(document) => document.poll(context) }
    }
    fn poll_for_settle(&mut self, context: Option<Context>, timers: bool) -> bool {
        match self { Self::Boa(document) => document.poll_for_settle(context, timers),
            #[cfg(feature = "ekojit")] Self::Ekojit(document) => document.poll_for_settle(context, timers) }
    }
}
