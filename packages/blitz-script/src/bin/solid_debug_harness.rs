use std::path::PathBuf;
use std::time::Duration;

use blitz_dom::{Document, DocumentConfig};
use blitz_script::{DebugController, ScriptDocument};
use blitz_traits::shell::{ColorScheme, Viewport};
use url::Url;

fn main() {
    let index_path = std::env::var_os("BLITZ_DEBUG_DOCUMENT")
        .or_else(|| std::env::var_os("BLITZ_SOLID_PROBE"))
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../target/solid-probe/index.html")
        })
        .canonicalize()
        .expect("Solid probe must be built before starting the debug harness");
    let html = std::fs::read_to_string(&index_path).expect("failed to read Solid probe");
    let base_url = Url::from_file_path(&index_path).expect("probe path must be a file URL");
    let mut document = ScriptDocument::from_html(
        &html,
        DocumentConfig {
            base_url: Some(base_url.to_string()),
            viewport: Some(Viewport::new(640, 480, 1.0, ColorScheme::Light)),
            ..Default::default()
        },
    );
    let mut control = DebugController::start_from_env(env!("CARGO_PKG_VERSION"))
        .expect("invalid debug-control configuration")
        .expect("TAURI_BLITZ_DRIVER is required")
        .with_cpu_screenshot(640, 480);

    document.execute_scripts();
    document.inner_mut().resolve(0.0);

    while !control.exit_requested() {
        match control.service_one(&mut document, Duration::from_millis(100)) {
            Ok(true) => {}
            Ok(false) => break,
            Err(_) => {
                document.poll(None);
            }
        }
    }
}
