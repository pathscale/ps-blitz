//! `location.assign`, `location.replace` and `location.reload`. `location`
//! was a plain data object, so a page calling any of the three threw.

use blitz_dom::{Document, DocumentConfig};
use blitz_script::ScriptDocument;
use blitz_traits::navigation::{NavigationOptions, NavigationProvider};
use std::sync::{Arc, Mutex};

fn text_of_selector(doc: &ScriptDocument, selector: &str) -> String {
    let inner = doc.inner();
    let node_id = inner
        .query_selector(selector)
        .unwrap()
        .unwrap_or_else(|| panic!("no node matching {selector}"));
    inner.get_node(node_id).unwrap().text_content()
}

#[derive(Default)]
struct RecordingNavigation {
    navigations: Mutex<Vec<String>>,
}

impl NavigationProvider for RecordingNavigation {
    fn navigate_to(&self, options: NavigationOptions) {
        self.navigations
            .lock()
            .unwrap()
            .push(options.url.to_string());
    }
}

#[test]
fn location_exposes_assign_replace_and_reload() {
    let navigation = Arc::new(RecordingNavigation::default());
    let mut doc = ScriptDocument::from_html(
        r#"
        <html><body>
            <div id="out"></div>
            <script>
                const out = document.getElementById("out");
                const shape = ["assign", "replace", "reload"]
                    .map((name) => name + ":" + typeof location[name])
                    .join("|");
                out.textContent = shape;
                location.assign("/next");
            </script>
        </body></html>
        "#,
        DocumentConfig {
            navigation_provider: Some(Arc::clone(&navigation) as Arc<dyn NavigationProvider>),
            base_url: Some("https://example.test/page".to_string()),
            ..Default::default()
        },
    );
    doc.execute_scripts();

    assert_eq!(
        text_of_selector(&doc, "#out"),
        "assign:function|replace:function|reload:function"
    );
    assert_eq!(
        navigation.navigations.lock().unwrap().as_slice(),
        ["https://example.test/next"],
        "location.assign must reach the navigation provider"
    );
}
