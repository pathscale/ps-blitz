//! Tests for the JavaScript DOM APIs exposed by blitz-script

use blitz_dom::{Document, DocumentConfig};
use blitz_script::ScriptDocument;
use blitz_traits::events::DomEvent;
use keyboard_types::Modifiers;
use std::sync::{Arc, Mutex};

fn doc_from_html(html: &str) -> ScriptDocument {
    let mut doc = ScriptDocument::from_html(html, DocumentConfig::default());
    doc.execute_scripts();
    doc
}

fn text_of_selector(doc: &ScriptDocument, selector: &str) -> String {
    let inner = doc.inner();
    let node_id = inner
        .query_selector(selector)
        .unwrap()
        .unwrap_or_else(|| panic!("no node matching {selector}"));
    inner.get_node(node_id).unwrap().text_content()
}

#[test]
fn executes_inline_scripts() {
    let doc = doc_from_html(
        r#"
        <html><body>
            <div id="root"></div>
            <script>
                const el = document.createElement("h1");
                el.textContent = "Hello from JS";
                document.getElementById("root").appendChild(el);
            </script>
        </body></html>
        "#,
    );
    assert_eq!(text_of_selector(&doc, "#root > h1"), "Hello from JS");
}

#[test]
fn scripts_run_in_document_order_and_share_globals() {
    let doc = doc_from_html(
        r#"
        <html><body>
            <div id="root"></div>
            <script>globalThis.counter = 1;</script>
            <script>globalThis.counter += 1;</script>
            <script>
                document.getElementById("root").textContent = `counter = ${counter}`;
            </script>
        </body></html>
        "#,
    );
    assert_eq!(text_of_selector(&doc, "#root"), "counter = 2");
}

#[test]
fn history_tracks_state_and_same_origin_urls() {
    let mut doc = ScriptDocument::from_html(
        r#"
        <html><body>
            <div id="out"></div>
            <script>
                const initialState = history.state;
                history.replaceState({ ...history.state, depth: 0 }, "");
                history.pushState({ page: 1 }, "", "/components?group=forms#input");
                const pushed = [history.length, history.state.page, location.pathname, location.search, location.hash];
                history.back();
                const restored = [history.state.depth, location.pathname, location.search, location.hash];
                document.getElementById("out").textContent = [initialState === null, ...pushed, ...restored].join("|");
            </script>
        </body></html>
        "#,
        DocumentConfig {
            base_url: Some("tauri://localhost/".into()),
            ..DocumentConfig::default()
        },
    );
    doc.execute_scripts();
    assert_eq!(
        text_of_selector(&doc, "#out"),
        "true|2|1|/components|?group=forms|#input|0|/||"
    );
}

#[test]
fn window_ipc_forwards_messages_to_the_embedder() {
    let received = Arc::new(Mutex::new(Vec::new()));
    let received_by_handler = Arc::clone(&received);
    let mut doc = ScriptDocument::from_html(
        r#"<script>window.ipc.postMessage(JSON.stringify({ cmd: "greet", value: 42 }));</script>"#,
        DocumentConfig::default(),
    );
    doc.set_ipc_handler(move |body| received_by_handler.lock().unwrap().push(body));
    doc.execute_scripts();

    assert_eq!(
        received.lock().unwrap().as_slice(),
        [r#"{"cmd":"greet","value":42}"#]
    );
}

#[test]
fn dom_tree_manipulation() {
    let doc = doc_from_html(
        r#"
        <html><body>
            <ul id="list"><li id="a">a</li><li id="c">c</li></ul>
            <script>
                const list = document.getElementById("list");
                const b = document.createElement("li");
                b.textContent = "b";
                list.insertBefore(b, document.getElementById("c"));

                // Move "a" to the end, then remove it
                const a = document.getElementById("a");
                list.appendChild(a);
                list.removeChild(a);

                const summary = document.createElement("div");
                summary.id = "summary";
                summary.textContent = [...list.childNodes].map((li) => li.textContent).join(",");
                document.body.appendChild(summary);
            </script>
        </body></html>
        "#,
    );
    assert_eq!(text_of_selector(&doc, "#summary"), "b,c");
    assert_eq!(text_of_selector(&doc, "#list"), "bc");
}

#[test]
fn attributes_and_properties() {
    let doc = doc_from_html(
        r#"
        <html><body>
            <div id="box" class="before" data-x="1"></div>
            <script>
                const box = document.getElementById("box");
                const results = [];
                results.push(box.getAttribute("class"));
                box.className = "after";
                results.push(box.getAttribute("class"));
                results.push(box.hasAttribute("data-x"));
                box.removeAttribute("data-x");
                results.push(box.hasAttribute("data-x"));
                box.setAttribute("title", "hello");
                results.push(box.getAttribute("title"));

                const out = document.createElement("div");
                out.id = "out";
                out.textContent = results.join("|");
                document.body.appendChild(out);
            </script>
        </body></html>
        "#,
    );
    assert_eq!(
        text_of_selector(&doc, "#out"),
        "before|after|true|false|hello"
    );
}

#[test]
fn dataset_reflects_data_attributes() {
    let doc = doc_from_html(
        r#"
        <html><body>
            <div id="box" data-user-id="42"></div>
            <script>
                const box = document.getElementById("box");
                const sameObject = box.dataset === box.dataset;
                const initial = box.dataset.userId;
                box.dataset.colorMode = "dark";
                const reflected = box.getAttribute("data-color-mode");
                const present = "colorMode" in box.dataset;
                const keys = Object.keys(box.dataset).sort().join(",");
                delete box.dataset.userId;
                const removed = !box.hasAttribute("data-user-id");
                const out = document.createElement("div");
                out.id = "dataset-out";
                out.textContent = [sameObject, initial, reflected, present, keys, removed].join("|");
                document.body.appendChild(out);
            </script>
        </body></html>
        "#,
    );
    assert_eq!(
        text_of_selector(&doc, "#dataset-out"),
        "true|42|dark|true|colorMode,userId|true"
    );
}

#[test]
fn class_list_reflects_the_class_attribute() {
    let doc = doc_from_html(
        r#"
        <html><body>
            <div id="box" class="one two"></div>
            <script>
                const box = document.getElementById("box");
                const sameObject = box.classList === box.classList;
                box.classList.add("three", "one");
                const forcedOff = box.classList.toggle("two", false);
                const toggledOn = box.classList.toggle("four");
                const replaced = box.classList.replace("three", "five");
                box.classList.remove("one");
                const out = document.createElement("div");
                out.id = "class-list-out";
                out.textContent = [
                    sameObject,
                    forcedOff,
                    toggledOn,
                    replaced,
                    box.className,
                    box.classList.length,
                    box.classList.item(0),
                    box.classList.contains("five"),
                    String(box.classList),
                ].join("|");
                document.body.appendChild(out);
            </script>
        </body></html>
        "#,
    );
    assert_eq!(
        text_of_selector(&doc, "#class-list-out"),
        "true|false|true|true|five four|2|five|true|five four"
    );
}

#[test]
fn structured_clone_copies_supported_values_and_cycles() {
    let doc = doc_from_html(
        r#"
        <html><body>
            <script>
                const source = {
                    nested: { value: 7 },
                    list: [1, 2],
                    date: new Date("2024-01-02T03:04:05Z"),
                    map: new Map([["key", { value: 9 }]]),
                    set: new Set(["a", "b"]),
                };
                source.self = source;
                const copy = structuredClone(source);
                copy.nested.value = 8;
                const out = document.createElement("div");
                out.id = "clone-out";
                out.textContent = [
                    copy !== source,
                    copy.self === copy,
                    source.nested.value,
                    copy.nested.value,
                    copy.list.join(","),
                    copy.date.toISOString(),
                    copy.map.get("key").value,
                    [...copy.set].join(","),
                ].join("|");
                document.body.appendChild(out);
            </script>
        </body></html>
        "#,
    );
    assert_eq!(
        text_of_selector(&doc, "#clone-out"),
        "true|true|7|8|1,2|2024-01-02T03:04:05.000Z|9|a,b"
    );
}

#[test]
fn web_crypto_fills_integer_typed_arrays() {
    let doc = doc_from_html(
        r#"
        <html><body>
            <script>
                const values = new Uint32Array(2);
                const returned = crypto.getRandomValues(values);
                const nullPrototype = Object.create(null);
                const out = document.createElement("div");
                out.id = "crypto-out";
                out.textContent = [
                    typeof crypto.getRandomValues,
                    returned === values,
                    values.length,
                    Number.isInteger(values[0]),
                    Object.getPrototypeOf(nullPrototype) === null,
                ].join("|");
                document.body.appendChild(out);
            </script>
        </body></html>
        "#,
    );
    assert_eq!(
        text_of_selector(&doc, "#crypto-out"),
        "function|true|2|true|true"
    );
}

#[test]
fn query_selectors() {
    let doc = doc_from_html(
        r#"
        <html><body>
            <div class="item">one</div>
            <div class="item special">two</div>
            <section><div class="item">three</div></section>
            <script>
                const out = document.createElement("div");
                out.id = "out";
                const all = document.querySelectorAll(".item").length;
                const special = document.querySelector(".item.special").textContent;
                const scoped = document.querySelector("section").querySelectorAll(".item").length;
                out.textContent = `${all}|${special}|${scoped}`;
                document.body.appendChild(out);
            </script>
        </body></html>
        "#,
    );
    assert_eq!(text_of_selector(&doc, "#out"), "3|two|1");
}

#[test]
fn inner_html() {
    let doc = doc_from_html(
        r#"
        <html><body>
            <div id="root"><span>old</span></div>
            <script>
                const root = document.getElementById("root");
                root.innerHTML = "<p class='msg'>new <b>content</b></p>";
            </script>
        </body></html>
        "#,
    );
    assert_eq!(text_of_selector(&doc, "#root .msg"), "new content");
    let inner = doc.inner();
    assert!(inner.query_selector("#root span").unwrap().is_none());
}

#[test]
fn template_content_exposes_parsed_children() {
    let doc = doc_from_html(
        r#"
        <html><body>
            <script>
                const template = document.createElement("template");
                template.innerHTML = "<span class='from-template'>hello</span>";
                const clone = template.content.firstChild.cloneNode(true);
                document.body.appendChild(clone);
            </script>
        </body></html>
        "#,
    );

    assert_eq!(text_of_selector(&doc, ".from-template"), "hello");
}

#[test]
fn click_event_listeners() {
    let mut doc = doc_from_html(
        r#"
        <html><body>
            <button id="btn">Click me</button>
            <div id="out">unclicked</div>
            <script>
                let clicks = 0;
                const btn = document.getElementById("btn");
                btn.addEventListener("click", (event) => {
                    clicks += 1;
                    const out = document.getElementById("out");
                    out.textContent = `clicked ${clicks} times; target=${event.target.tagName}; ct=${event.currentTarget.id}`;
                });
            </script>
        </body></html>
        "#,
    );

    let click_event = {
        let inner = doc.inner();
        let btn_id = inner.query_selector("#btn").unwrap().unwrap();
        DomEvent::new(
            btn_id,
            inner
                .get_node(btn_id)
                .unwrap()
                .synthetic_click_event(Modifiers::empty()),
        )
    };
    doc.dispatch_dom_event(click_event.clone());
    assert_eq!(
        text_of_selector(&doc, "#out"),
        "clicked 1 times; target=BUTTON; ct=btn"
    );
    doc.dispatch_dom_event(click_event);
    assert_eq!(
        text_of_selector(&doc, "#out"),
        "clicked 2 times; target=BUTTON; ct=btn"
    );
}

#[test]
fn click_events_bubble_to_document() {
    let mut doc = doc_from_html(
        r#"
        <html><body>
            <button id="btn">Click me</button>
            <div id="out">unhandled</div>
            <script>
                document.addEventListener("click", (event) => {
                    document.getElementById("out").textContent =
                        `${event.target.tagName}|${event.currentTarget === document}`;
                });
            </script>
        </body></html>
        "#,
    );

    let click_event = {
        let inner = doc.inner();
        let btn_id = inner.query_selector("#btn").unwrap().unwrap();
        DomEvent::new(
            btn_id,
            inner
                .get_node(btn_id)
                .unwrap()
                .synthetic_click_event(Modifiers::empty()),
        )
    };
    doc.dispatch_dom_event(click_event);

    assert_eq!(text_of_selector(&doc, "#out"), "BUTTON|true");
}

#[test]
fn click_events_bubble_and_stop_propagation() {
    let mut doc = doc_from_html(
        r#"
        <html><body>
            <div id="outer"><div id="middle"><button id="inner">hi</button></div></div>
            <div id="out"></div>
            <script>
                const log = [];
                const record = (name) => () => {
                    log.push(name);
                    document.getElementById("out").textContent = log.join(",");
                };
                document.getElementById("outer").addEventListener("click", record("outer"));
                document.getElementById("middle").addEventListener("click", (event) => {
                    record("middle")();
                    event.stopPropagation();
                });
                document.getElementById("inner").addEventListener("click", record("inner"));
            </script>
        </body></html>
        "#,
    );

    let click_event = {
        let inner = doc.inner();
        let btn_id = inner.query_selector("#inner").unwrap().unwrap();
        DomEvent::new(
            btn_id,
            inner
                .get_node(btn_id)
                .unwrap()
                .synthetic_click_event(Modifiers::empty()),
        )
    };
    doc.dispatch_dom_event(click_event);

    // "outer" should not be reached because "middle" stops propagation
    assert_eq!(text_of_selector(&doc, "#out"), "inner,middle");
}

#[test]
fn microtasks_run_after_script_execution() {
    let doc = doc_from_html(
        r#"
        <html><body>
            <div id="out">pending</div>
            <script>
                Promise.resolve()
                    .then(() => "microtask")
                    .then((value) => {
                        document.getElementById("out").textContent = value;
                    });
            </script>
        </body></html>
        "#,
    );
    assert_eq!(text_of_selector(&doc, "#out"), "microtask");
}

#[test]
fn timers_run_on_poll() {
    let mut doc = doc_from_html(
        r#"
        <html><body>
            <div id="out">pending</div>
            <script>
                setTimeout((suffix) => {
                    document.getElementById("out").textContent = "timer ran " + suffix;
                }, 5, "with args");
            </script>
        </body></html>
        "#,
    );

    assert_eq!(text_of_selector(&doc, "#out"), "pending");
    std::thread::sleep(std::time::Duration::from_millis(20));
    let ran = doc.poll(None);
    assert!(ran);
    assert_eq!(text_of_selector(&doc, "#out"), "timer ran with args");
}

#[test]
fn request_animation_frame_runs_on_poll() {
    let mut doc = doc_from_html(
        r#"
        <html><body>
            <div id="out">pending</div>
            <script>
                requestAnimationFrame(() => {
                    document.getElementById("out").textContent = "frame";
                });
            </script>
        </body></html>
        "#,
    );

    std::thread::sleep(std::time::Duration::from_millis(30));
    doc.poll(None);
    assert_eq!(text_of_selector(&doc, "#out"), "frame");
}

#[test]
fn input_value_property() {
    let doc = doc_from_html(
        r#"
        <html><body>
            <input id="field" value="initial">
            <div id="out"></div>
            <script>
                const field = document.getElementById("field");
                const before = field.value;
                field.value = "updated";
                document.getElementById("out").textContent = `${before}|${field.value}`;
            </script>
        </body></html>
        "#,
    );
    assert_eq!(text_of_selector(&doc, "#out"), "initial|updated");
}

#[test]
fn constructed_events_dispatch_and_bubble() {
    let doc = doc_from_html(
        r#"
        <html><body>
            <input id="field">
            <div id="out"></div>
            <script>
                const field = document.getElementById("field");
                const out = document.getElementById("out");
                field.addEventListener("input", (event) => {
                    out.textContent = [
                        event instanceof Event,
                        event.target.id,
                        event.bubbles,
                        event.cancelable,
                        event.composed,
                        event.isTrusted,
                        event.currentTarget.id,
                    ].join("|");
                    event.preventDefault();
                });
                document.addEventListener("input", () => out.textContent += "|document");
                const accepted = field.dispatchEvent(new Event("input", {
                    bubbles: true,
                    cancelable: true,
                    composed: true,
                }));
                out.textContent += `|${accepted}`;
            </script>
        </body></html>
        "#,
    );
    assert_eq!(
        text_of_selector(&doc, "#out"),
        "true|field|true|true|true|false|field|document|false"
    );
}

#[test]
fn dom_interface_instanceof_checks_match_node_types_and_tags() {
    let doc = doc_from_html(
        r#"
        <html><head></head><body>
            <input id="field"><div id="out"></div>
            <script>
                const head = document.querySelector("head");
                const body = document.body;
                const field = document.getElementById("field");
                document.getElementById("out").textContent = [
                    document instanceof Node,
                    document instanceof Document,
                    document instanceof HTMLDocument,
                    body instanceof Node,
                    body instanceof Element,
                    body instanceof HTMLElement,
                    body instanceof HTMLBodyElement,
                    body instanceof HTMLHeadElement,
                    head instanceof HTMLHeadElement,
                    field instanceof HTMLInputElement,
                    field instanceof HTMLTextAreaElement,
                ].join("|");
            </script>
        </body></html>
        "#,
    );
    assert_eq!(
        text_of_selector(&doc, "#out"),
        "true|true|true|true|true|true|true|false|true|true|false"
    );
}

#[test]
fn checkbox_click_fires_input_and_change_events() {
    let mut doc = doc_from_html(
        r#"
        <html><body>
            <input type="checkbox" id="check">
            <div id="out"></div>
            <script>
                const check = document.getElementById("check");
                const log = [];
                check.addEventListener("input", () => log.push(`input:${check.checked}`));
                check.addEventListener("change", () => {
                    log.push(`change:${check.checked}`);
                    document.getElementById("out").textContent = log.join(",");
                });
            </script>
        </body></html>
        "#,
    );

    // Resolve style/layout: this constructs the checkbox's internal state
    // (as would happen before rendering in a windowed application)
    doc.inner_mut().resolve(0.0);

    let click_event = {
        let inner = doc.inner();
        let check_id = inner.query_selector("#check").unwrap().unwrap();
        DomEvent::new(
            check_id,
            inner
                .get_node(check_id)
                .unwrap()
                .synthetic_click_event(Modifiers::empty()),
        )
    };
    doc.dispatch_dom_event(click_event);
    assert_eq!(text_of_selector(&doc, "#out"), "input:true,change:true");
}

#[test]
fn dom_content_loaded_and_window_load_fire() {
    let doc = doc_from_html(
        r#"
        <html><body>
            <div id="out"></div>
            <script>
                const log = [];
                document.addEventListener("DOMContentLoaded", () => log.push("dcl"));
                window.addEventListener("load", () => {
                    log.push("load");
                    document.getElementById("out").textContent = log.join(",");
                });
            </script>
        </body></html>
        "#,
    );
    assert_eq!(text_of_selector(&doc, "#out"), "dcl,load");
}

#[test]
fn on_event_idl_properties_are_dispatched() {
    let mut doc = doc_from_html(
        r#"
        <html><body>
            <button id="btn">go</button>
            <div id="out"></div>
            <script>
                document.getElementById("btn").onclick = (event) => {
                    document.getElementById("out").textContent = `onclick:${event.type}`;
                };
            </script>
        </body></html>
        "#,
    );

    let click_event = {
        let inner = doc.inner();
        let btn_id = inner.query_selector("#btn").unwrap().unwrap();
        DomEvent::new(
            btn_id,
            inner
                .get_node(btn_id)
                .unwrap()
                .synthetic_click_event(Modifiers::empty()),
        )
    };
    doc.dispatch_dom_event(click_event);
    assert_eq!(text_of_selector(&doc, "#out"), "onclick:click");
}

#[test]
fn node_wrappers_have_stable_identity() {
    let doc = doc_from_html(
        r##"
        <html><body>
            <div id="root"><span id="child">x</span></div>
            <div id="out"></div>
            <script>
                const root1 = document.getElementById("root");
                const root2 = document.querySelector("#root");
                root1.expando = "kept";
                const sameObject = root1 === root2;
                const viaParent = document.getElementById("child").parentNode;
                document.getElementById("out").textContent =
                    `${sameObject}|${viaParent === root1}|${viaParent.expando}`;
            </script>
        </body></html>
        "##,
    );
    assert_eq!(text_of_selector(&doc, "#out"), "true|true|kept");
}

#[test]
fn style_bindings() {
    let doc = doc_from_html(
        r#"
        <html><body>
            <div id="box" style="color: red;"></div>
            <div id="out"></div>
            <script>
                const box = document.getElementById("box");
                const before = box.style.cssText;
                box.style.setProperty("background-color", "blue");
                const bg = box.style.getPropertyValue("background-color");
                document.getElementById("out").textContent = `${before}|${bg}`;
            </script>
        </body></html>
        "#,
    );
    assert_eq!(text_of_selector(&doc, "#out"), "color: red;|blue");
}

#[test]
fn eval_json_returns_embedder_friendly_results() {
    let mut doc = doc_from_html("<html><body></body></html>");

    let result = doc
        .eval_json("({ greeting: 'hello', count: 2 })")
        .expect("script should evaluate");

    assert_eq!(
        result,
        serde_json::json!({ "greeting": "hello", "count": 2 })
    );
}

#[test]
fn embedder_poll_hook_runs_after_document_scripts() {
    let mut doc = ScriptDocument::from_html(
        r#"
        <div id="out">waiting</div>
        <script>window.fromDocument = "ready";</script>
        "#,
        DocumentConfig::default(),
    );
    doc.set_poll_hook(|document, _| {
        document.eval(
            "document.getElementById('out').textContent = window.fromDocument + ' from hook';",
        );
        true
    });

    assert!(doc.poll(None));
    assert_eq!(text_of_selector(&doc, "#out"), "ready from hook");
}
