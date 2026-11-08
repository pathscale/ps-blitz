use blitz_dom::{BaseDocument, DocumentConfig};
use blitz_script::native::{Guest, RuntimeBridge};
use serde_json::json;

struct NoHost;
impl RuntimeBridge for NoHost {}

#[test]
fn keyboard_event_initializer_fields_reach_listeners() {
    let mut guest = Guest::new(NoHost);
    let mut document = BaseDocument::new(DocumentConfig::default());
    let result = guest
        .evaluate(
            &mut document,
            r#"
            const target = new EventTarget();
            let observed = null;
            target.addEventListener('keydown', function(event) {
                observed = [event.location, event.repeat, event.isComposing];
            });
            target.dispatchEvent(new KeyboardEvent('keydown', {
                location: 3,
                repeat: true,
                isComposing: true,
            }));
            const defaults = new KeyboardEvent('keydown');
            const converted = new KeyboardEvent('keydown', {
                get location() { return -1.5; },
                repeat: '',
                isComposing: 1,
            });
            [observed,
             [defaults.location, defaults.repeat, defaults.isComposing],
             [converted.location, converted.repeat, converted.isComposing]];
            "#,
        )
        .unwrap()
        .to_json()
        .unwrap();
    assert_eq!(
        result,
        json!([[3, true, true], [0, false, false], [4_294_967_295_u32, false, true]])
    );
}
