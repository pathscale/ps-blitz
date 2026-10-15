use blitz_dom::DocumentConfig;
use blitz_html::HtmlDocument;
use blitz_shell::{BlitzApplication, BlitzShellProxy, WindowConfig, create_default_event_loop};

use crate::{DemoWidget, FEATURES, STYLES, limits};

#[cfg(feature = "vello")]
/// Create renderer
fn create_renderer() -> anyrender_vello::VelloWindowRenderer {
    use anyrender_vello::{VelloRendererOptions, VelloWindowRenderer};
    VelloWindowRenderer::with_options(
        VelloRendererOptions::default()
            .features(FEATURES)
            .limits(limits()),
    )
}

#[cfg(all(feature = "vello-hybrid", not(feature = "vello")))]
/// Create renderer
fn create_renderer() -> anyrender_vello_hybrid::VelloHybridWindowRenderer {
    use anyrender_vello_hybrid::{VelloHybridRendererOptions, VelloHybridWindowRenderer};
    VelloHybridWindowRenderer::with_options(
        VelloHybridRendererOptions::default()
            .features(FEATURES)
            .limits(limits()),
    )
}

pub fn launch_html() {
    // Create custom paint source and register it with the renderer
    let event_loop = create_default_event_loop();
    let (proxy, reciever) = BlitzShellProxy::new(event_loop.create_proxy());
    let mut doc = HtmlDocument::from_html(
        &HTML.replace("{{STYLES_PLACEHOLDER}}", STYLES),
        DocumentConfig::default(),
    );
    let doc_id = blitz_dom::Document::id(&doc);
    let wake_proxy = proxy.clone();
    let demo_widget = Box::new(DemoWidget::new(move || {
        wake_proxy.send_event(blitz_shell::BlitzShellEvent::RequestRedraw { doc_id });
    }));

    let renderer = create_renderer();

    // Set a custom widget on a `<object>` element
    let canvas_node_id = doc.query_selector("#demo-canvas").unwrap().unwrap();
    doc.mutate().set_custom_widget(canvas_node_id, demo_widget);

    // Create the Winit application and window
    let mut application = BlitzApplication::new(proxy, reciever);
    let window = WindowConfig::new(Box::new(doc), renderer);
    application.add_window(window);

    // Run event loop
    event_loop.run_app(application).unwrap()
}

static HTML: &str = r#"
    <!DOCTYPE html>
    <html>
    <head>
        <style type="text/css">
            {{STYLES_PLACEHOLDER}}
        </style>
    </head>
    <body>
        <main id="main">
            <div id="overlay">
                <h2>Overlay</h2>
                <p>This overlay demonstrates that the custom WGPU content can be rendered beneath layers of HTML content</p>
            </div>
            <div id="underlay">
                <h2>Underlay</h2>
                <p>This underlay demonstrates that the custom WGPU content can be rendered above layers and blended with the content underneath</p>
            </div>
            <header><h1>Blitz WGPU Demo</h1></header>
            <div id="canvas-container">
                <object id="demo-canvas" />
            </div>
        </main>
    </body>
    </html>
"#;
