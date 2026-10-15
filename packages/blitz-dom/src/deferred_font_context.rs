use parley::FontContext;
use std::ops::{Deref, DerefMut};
#[cfg(all(feature = "system-fonts", not(target_arch = "wasm32")))]
use std::sync::OnceLock;
use std::sync::{LockResult, Mutex, MutexGuard, PoisonError};

struct FontContextState {
    context: FontContext,
    system_fonts_pending: bool,
    #[cfg(all(feature = "system-fonts", not(target_arch = "wasm32")))]
    use_system_font_template: bool,
}

pub(crate) struct DeferredFontContext {
    inner: Mutex<FontContextState>,
}

pub(crate) struct DeferredFontContextGuard<'a> {
    guard: MutexGuard<'a, FontContextState>,
}

impl Deref for DeferredFontContextGuard<'_> {
    type Target = FontContext;

    fn deref(&self) -> &Self::Target {
        &self.guard.context
    }
}

impl DerefMut for DeferredFontContextGuard<'_> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.guard.context
    }
}

#[cfg(all(feature = "system-fonts", not(target_arch = "wasm32")))]
static SYSTEM_FONT_CONTEXT: OnceLock<Mutex<FontContext>> = OnceLock::new();

#[cfg(all(feature = "system-fonts", not(target_arch = "wasm32")))]
fn system_font_context() -> FontContext {
    let template = SYSTEM_FONT_CONTEXT.get_or_init(|| Mutex::new(crate::build_font_context(true)));

    match template.lock() {
        Ok(context) => context.clone(),
        Err(poisoned) => poisoned.into_inner().clone(),
    }
}

impl DeferredFontContext {
    pub(crate) fn cloned_config(&self) -> (Option<FontContext>, bool) {
        let state = self.inner.lock().unwrap();
        #[cfg(all(feature = "system-fonts", not(target_arch = "wasm32")))]
        if state.system_fonts_pending && state.use_system_font_template {
            return (None, true);
        }
        (Some(state.context.clone()), state.system_fonts_pending)
    }

    pub(crate) fn new(
        context: FontContext,
        load_system_fonts: bool,
        use_system_font_template: bool,
    ) -> Self {
        #[cfg(not(all(feature = "system-fonts", not(target_arch = "wasm32"))))]
        let _ = use_system_font_template;
        Self {
            inner: Mutex::new(FontContextState {
                context,
                system_fonts_pending: load_system_fonts
                    && cfg!(all(feature = "system-fonts", not(target_arch = "wasm32"))),
                #[cfg(all(feature = "system-fonts", not(target_arch = "wasm32")))]
                use_system_font_template,
            }),
        }
    }

    pub(crate) fn lock(&self) -> LockResult<DeferredFontContextGuard<'_>> {
        let mut guard = match self.inner.lock() {
            Ok(guard) => guard,
            Err(poisoned) => {
                return Err(PoisonError::new(DeferredFontContextGuard {
                    guard: poisoned.into_inner(),
                }));
            }
        };

        if guard.system_fonts_pending {
            #[cfg(all(feature = "system-fonts", not(target_arch = "wasm32")))]
            {
                if guard.use_system_font_template {
                    guard.context = system_font_context();
                } else {
                    guard.context.collection.load_system_fonts();
                }
            }
            guard.system_fonts_pending = false;
        }

        Ok(DeferredFontContextGuard { guard })
    }
}
