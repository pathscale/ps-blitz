//! Tests for the JavaScript DOM APIs exposed by blitz-script

use blitz_dom::{Document, DocumentConfig};
use blitz_script::ScriptDocument;
use blitz_traits::events::{DomEvent, DomEventData};
use blitz_traits::shell::{ColorScheme, Viewport};
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
fn pointer_capture_methods_retarget_pointer_events() {
    let mut doc = doc_from_html(
        r#"
        <html><body>
            <button id="capture">capture</button><div id="other"></div><div id="out"></div>
            <script>
                const capture = document.getElementById("capture");
                const out = document.getElementById("out");
                capture.addEventListener("pointerdown", (event) => {
                    capture.setPointerCapture(event.pointerId);
                    out.textContent = `down:${event.pointerId}:${capture.hasPointerCapture(event.pointerId)}`;
                });
                capture.addEventListener("pointermove", (event) => {
                    out.textContent += `|move:${event.pointerId}`;
                    capture.releasePointerCapture(event.pointerId);
                });
            </script>
        </body></html>
        "#,
    );

    let (capture_id, other_id, pointer) = {
        let inner = doc.inner();
        let capture_id = inner.query_selector("#capture").unwrap().unwrap();
        let other_id = inner.query_selector("#other").unwrap().unwrap();
        let pointer = match inner
            .get_node(capture_id)
            .unwrap()
            .synthetic_click_event(Modifiers::empty())
        {
            DomEventData::Click(pointer) => pointer,
            _ => unreachable!(),
        };
        (capture_id, other_id, pointer)
    };

    doc.dispatch_dom_event(DomEvent::new(
        capture_id,
        DomEventData::PointerDown(pointer.clone()),
    ));
    doc.dispatch_dom_event(DomEvent::new(other_id, DomEventData::PointerMove(pointer)));

    assert_eq!(text_of_selector(&doc, "#out"), "down:1:true|move:1");
}

#[test]
fn matches_and_closest_follow_the_element_ancestor_chain() {
    let doc = doc_from_html(
        r#"
        <div id="root" data-drag><button><span id="target">target</span></button></div>
        <div id="out"></div>
        <script>
            const target = document.getElementById("target");
            const closest = target.closest("[data-drag]");
            document.getElementById("out").textContent = [
                target.matches("span"),
                target.matches("button"),
                closest && closest.id,
                target.closest("[data-missing]") === null,
            ].join("|");
        </script>
        "#,
    );
    assert_eq!(text_of_selector(&doc, "#out"), "true|false|root|true");
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
fn window_dimensions_follow_the_current_viewport() {
    let mut doc = ScriptDocument::from_html(
        r#"<div id="out"></div><script>
            document.getElementById("out").textContent =
                [innerWidth, innerHeight, outerWidth, outerHeight, devicePixelRatio].join("|");
        </script>"#,
        DocumentConfig {
            viewport: Some(Viewport::new(800, 600, 2.0, ColorScheme::Light)),
            ..DocumentConfig::default()
        },
    );
    doc.execute_scripts();
    assert_eq!(text_of_selector(&doc, "#out"), "400|300|400|300|2");

    doc.inner_mut()
        .set_viewport(Viewport::new(1000, 700, 2.0, ColorScheme::Light));
    doc.eval(
        r#"document.getElementById("out").textContent =
            [innerWidth, innerHeight, outerWidth, outerHeight, devicePixelRatio].join("|");"#,
    );
    assert_eq!(text_of_selector(&doc, "#out"), "500|350|500|350|2");
}

#[test]
fn element_scroll_metrics_and_offsets_follow_layout() {
    let mut doc = ScriptDocument::from_html(
        r#"
        <div id="strip" style="width: 100px; height: 50px; overflow: auto">
            <div style="width: 300px; height: 150px"></div>
        </div>
        "#,
        DocumentConfig {
            viewport: Some(Viewport::new(400, 200, 1.0, ColorScheme::Light)),
            ..DocumentConfig::default()
        },
    );
    doc.execute_scripts();
    doc.inner_mut().resolve(0.0);

    let metrics = doc
        .eval_json(
            r#"
            const strip = document.getElementById("strip");
            const child = strip.children[0];
            strip.scrollLeft = 80;
            strip.scrollTop = 30;
            ({
                clientWidth: strip.clientWidth,
                clientHeight: strip.clientHeight,
                scrollWidth: strip.scrollWidth,
                scrollHeight: strip.scrollHeight,
                scrollLeft: strip.scrollLeft,
                scrollTop: strip.scrollTop,
                stripLeft: strip.getBoundingClientRect().left,
                stripTop: strip.getBoundingClientRect().top,
                childLeft: child.getBoundingClientRect().left,
                childTop: child.getBoundingClientRect().top,
            })
            "#,
        )
        .expect("scroll metrics should evaluate");

    assert_eq!(
        metrics,
        serde_json::json!({
            "clientWidth": 100.0,
            "clientHeight": 50.0,
            "scrollWidth": 300.0,
            "scrollHeight": 150.0,
            "scrollLeft": 80.0,
            "scrollTop": 30.0,
            "stripLeft": 8.0,
            "stripTop": 8.0,
            "childLeft": -72.0,
            "childTop": -22.0,
        })
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
fn click_events_expose_the_composed_path() {
    let mut doc = doc_from_html(
        r#"
        <html><body>
            <div id="outer"><button id="btn">Click me</button></div>
            <div id="out">missing</div>
            <script>
                const btn = document.getElementById("btn");
                btn.addEventListener("click", (event) => {
                    const path = event.composedPath();
                    document.getElementById("out").textContent = [
                        path[0] === btn,
                        path.indexOf(document) >= 0,
                        path[path.length - 1] === window,
                    ].join("|");
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

    assert_eq!(text_of_selector(&doc, "#out"), "true|true|true");
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
fn compare_document_position_reports_tree_order_and_containment() {
    let doc = doc_from_html(
        r#"
        <html><body>
            <main id="parent"><span id="first"></span><span id="second"></span></main>
            <div id="out"></div>
            <script>
                const parent = document.getElementById("parent");
                const first = document.getElementById("first");
                const second = document.getElementById("second");
                document.getElementById("out").textContent = [
                    first.compareDocumentPosition(first),
                    first.compareDocumentPosition(second),
                    second.compareDocumentPosition(first),
                    parent.compareDocumentPosition(first),
                    first.compareDocumentPosition(parent),
                    Node.DOCUMENT_POSITION_FOLLOWING,
                    Node.DOCUMENT_POSITION_PRECEDING,
                    Node.DOCUMENT_POSITION_CONTAINED_BY,
                    Node.DOCUMENT_POSITION_CONTAINS,
                ].join("|");
            </script>
        </body></html>
        "#,
    );
    assert_eq!(text_of_selector(&doc, "#out"), "0|4|2|20|10|4|2|16|8");
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

#[test]
fn assigning_a_style_property_reaches_the_document() {
    // `element.style.height = "70px"` was a no-op.
    //
    // `CSSStyleDeclaration` in a browser carries a named accessor for every CSS
    // property. This binding defined only `cssText`, `setProperty`,
    // `removeProperty` and `getPropertyValue`, and `element.style` returned a
    // fresh object per access, so the assignment set a plain JS property on a
    // throwaway object and was discarded without a word.
    //
    // What it cost: the composer measures its content and writes its own
    // height, so an autosizing prompt grew its container and never grew the
    // field inside it. The text went to a second line that could not be seen.
    let mut doc = ScriptDocument::from_html(
        r#"<div id="box" style="color: red">text</div>"#,
        DocumentConfig::default(),
    );
    doc.execute_scripts();

    let result = doc
        .eval_json(
            r#"
            const box = document.getElementById("box");
            box.style.height = "70px";
            box.style.maxHeight = "120px";
            const before = box.getAttribute("style");
            box.style.height = "";
            ({
              attr: before,
              readBack: box.style.maxHeight,
              afterClear: box.getAttribute("style"),
              apiStillWorks: (() => {
                box.style.setProperty("width", "40px");
                return box.style.getPropertyValue("width");
              })(),
            })
            "#,
        )
        .expect("style assignment should evaluate");

    let attr = result["attr"].as_str().unwrap_or_default();
    assert!(
        attr.contains("height: 70px"),
        "height must reach the style attribute: {result}"
    );
    // camelCase becomes kebab-case, or the declaration is not CSS.
    assert!(
        attr.contains("max-height: 120px"),
        "maxHeight must be written as max-height: {result}"
    );
    assert_eq!(
        result["readBack"].as_str(),
        Some("120px"),
        "a property must read back: {result}"
    );
    assert!(
        !result["afterClear"]
            .as_str()
            .unwrap_or_default()
            .contains("height: 70px"),
        "assigning an empty string must remove the declaration: {result}"
    );
    assert_eq!(
        result["apiStillWorks"].as_str(),
        Some("40px"),
        "setProperty and getPropertyValue must not be shadowed by the proxy: {result}"
    );
}

#[test]
fn a_fixed_overlay_appended_by_script_covers_the_viewport() {
    // How every modal in an embedding app is opened: build the backdrop, move
    // it under `body`, let `position: fixed; inset: 0` size it. If the append
    // does not re-run layout against the viewport the backdrop keeps whatever
    // box it was measured with while detached, and the dialog paints as a strip
    // across the top of the window instead of covering it.
    let mut doc = ScriptDocument::from_html(
        r#"
        <style>
          html, body { height: 100%; margin: 0 }
          .overlay { position: fixed; top: 0; right: 0; bottom: 0; left: 0 }
        </style>
        <div id="app" style="height: 40px"></div>
        "#,
        DocumentConfig {
            viewport: Some(Viewport::new(800, 600, 1.0, ColorScheme::Light)),
            ..DocumentConfig::default()
        },
    );
    doc.execute_scripts();
    doc.inner_mut().resolve(0.0);

    let rect = doc
        .eval_json(
            r#"
            const overlay = document.createElement("div");
            overlay.className = "overlay";
            document.body.appendChild(overlay);
            const box = overlay.getBoundingClientRect();
            ({ width: box.width, height: box.height, top: box.top, left: box.left })
            "#,
        )
        .expect("overlay geometry should evaluate");

    assert_eq!(
        rect,
        serde_json::json!({ "width": 800.0, "height": 600.0, "top": 0.0, "left": 0.0 })
    );
}

#[test]
fn appending_an_attached_node_moves_it_rather_than_sharing_it() {
    // `appendChild` on a node that already has a parent is a *move*: the DOM
    // detaches it first. A modal that relocates its own subtree under `body` to
    // escape a containing block relies on exactly that. Leaving the node in both
    // child lists lays it out twice, once in flow where it came from, which is
    // how a full-screen backdrop paints as a strip across the old parent.
    let mut doc = ScriptDocument::from_html(
        r#"<div id="host"><div id="panel">panel</div></div>"#,
        DocumentConfig {
            viewport: Some(Viewport::new(800, 600, 1.0, ColorScheme::Light)),
            ..DocumentConfig::default()
        },
    );
    doc.execute_scripts();
    doc.inner_mut().resolve(0.0);

    let moved = doc
        .eval_json(
            r#"
            const panel = document.getElementById("panel");
            document.body.appendChild(panel);
            ({
                hostChildren: document.getElementById("host").children.length,
                parentIsBody: panel.parentNode === document.body,
                bodyHasPanelOnce:
                    Array.prototype.filter.call(document.body.children, (c) => c.id === "panel").length,
            })
            "#,
        )
        .expect("reparent should evaluate");

    assert_eq!(
        moved,
        serde_json::json!({
            "hostChildren": 0,
            "parentIsBody": true,
            "bodyHasPanelOnce": 1,
        })
    );
}

#[test]
fn parent_node_append_moves_a_subtree_and_takes_strings() {
    // `document.body.append(node)` is how a dialog escapes an ancestor that
    // would otherwise be the containing block for its `position: fixed`
    // backdrop. Without `append` the call threw, the lifecycle hook that made
    // it unwound, and the dialog stayed where it was built — the backdrop then
    // painted inside that ancestor as a strip rather than over the window.
    let mut doc = ScriptDocument::from_html(
        r#"<div id="host"><div id="panel">panel</div></div><div id="sink"></div>"#,
        DocumentConfig {
            viewport: Some(Viewport::new(800, 600, 1.0, ColorScheme::Light)),
            ..DocumentConfig::default()
        },
    );
    doc.execute_scripts();
    doc.inner_mut().resolve(0.0);

    let result = doc
        .eval_json(
            r#"
            const panel = document.getElementById("panel");
            const sink = document.getElementById("sink");
            sink.append(panel, " tail");
            const list = document.createElement("div");
            list.append("b", "c");
            list.prepend("a");
            document.body.append(list);
            const wiped = document.createElement("div");
            wiped.append("gone");
            wiped.replaceChildren("kept");
            ({
                hostChildren: document.getElementById("host").children.length,
                sinkText: sink.textContent,
                listText: list.textContent,
                wipedText: wiped.textContent,
            })
            "#,
        )
        .expect("append should evaluate");

    assert_eq!(
        result,
        serde_json::json!({
            "hostChildren": 0,
            "sinkText": "panel tail",
            "listText": "abc",
            "wipedText": "kept",
        })
    );
}
/// A shell that records what script asked it to put on the clipboard.
#[derive(Default)]
struct RecordingClipboard {
    written: Mutex<Vec<String>>,
}

impl blitz_traits::shell::ShellProvider for RecordingClipboard {
    fn set_clipboard_text(&self, text: String) -> Result<(), blitz_traits::shell::ClipboardError> {
        self.written.lock().unwrap().push(text);
        Ok(())
    }
}

#[test]
fn navigator_clipboard_write_text_reaches_the_shell() {
    // Every "copy" button in an embedding app goes through this one call. With
    // no `navigator.clipboard` the property lookup threw, the usual try/catch
    // swallowed it, and the button reported success while copying nothing.
    let clipboard = Arc::new(RecordingClipboard::default());
    let mut doc = ScriptDocument::from_html(
        r#"<div id="out"></div>"#,
        DocumentConfig {
            viewport: Some(Viewport::new(400, 200, 1.0, ColorScheme::Light)),
            ..DocumentConfig::default()
        },
    );
    doc.inner_mut().set_shell_provider(clipboard.clone());
    doc.execute_scripts();

    doc.eval(
        r#"navigator.clipboard
             .writeText("session-4f21")
             .then(() => { document.getElementById("out").textContent = "copied" });"#,
    );
    doc.poll(None);

    assert_eq!(*clipboard.written.lock().unwrap(), vec!["session-4f21"]);
    assert_eq!(text_of_selector(&doc, "#out"), "copied");
}
#[test]
fn document_get_selection_is_callable_when_nothing_is_selected() {
    // The failure this guards is not a wrong value, it is a thrown TypeError:
    // `document.getSelection` was undefined, so calling it aborted whichever
    // copy or keydown handler reached for it, and the visible symptom was a
    // button that did nothing at all.
    let mut doc = ScriptDocument::from_html(
        r#"<p id="body">some text</p>"#,
        DocumentConfig {
            viewport: Some(Viewport::new(400, 200, 1.0, ColorScheme::Light)),
            ..DocumentConfig::default()
        },
    );
    doc.execute_scripts();
    doc.inner_mut().resolve(0.0);

    let state = doc
        .eval_json(
            r#"
            const selection = document.getSelection();
            ({
                text: selection.toString(),
                rangeCount: selection.rangeCount,
                collapsed: selection.isCollapsed,
                optionalChain: document.getSelection()?.toString() ?? "",
                restoreIsSafe: (() => {
                    const previous = selection.rangeCount ? selection.getRangeAt(0) : null;
                    selection.removeAllRanges();
                    if (previous) selection.addRange(previous);
                    return true;
                })(),
            })
            "#,
        )
        .expect("getSelection should evaluate");

    assert_eq!(
        state,
        serde_json::json!({
            "text": "",
            "rangeCount": 0,
            "collapsed": true,
            "optionalChain": "",
            "restoreIsSafe": true,
        })
    );
}
#[test]
fn box_metrics_are_reported_in_unzoomed_css_pixels() {
    // `scrollHeight` is defined in CSS pixels, so a zoomed element must report
    // the same number an unzoomed one would. Returning the zoomed layout height
    // breaks the standard autosize idiom — measure `scrollHeight`, write it back
    // into `style.height` — because the write is zoomed a second time and the
    // element grows by the zoom factor on every pass. `getBoundingClientRect`
    // is the exception and stays in zoomed viewport coordinates.
    let mut doc = ScriptDocument::from_html(
        r#"
        <div id="root" style="zoom: 2">
            <div id="box" style="width: 100px; height: 50px; overflow: auto">
                <div style="width: 300px; height: 150px"></div>
            </div>
        </div>
        "#,
        DocumentConfig {
            viewport: Some(Viewport::new(2000, 1000, 1.0, ColorScheme::Light)),
            ..DocumentConfig::default()
        },
    );
    doc.execute_scripts();
    doc.inner_mut().resolve(0.0);

    let metrics = doc
        .eval_json(
            r#"
            const box = document.getElementById("box");
            ({
                clientWidth: box.clientWidth,
                clientHeight: box.clientHeight,
                scrollWidth: box.scrollWidth,
                scrollHeight: box.scrollHeight,
                rectHeight: box.getBoundingClientRect().height,
            })
            "#,
        )
        .expect("zoomed box metrics should evaluate");

    assert_eq!(
        metrics,
        serde_json::json!({
            "clientWidth": 100.0,
            "clientHeight": 50.0,
            "scrollWidth": 300.0,
            "scrollHeight": 150.0,
            "rectHeight": 100.0,
        })
    );
}

#[test]
fn repeated_resolves_do_not_grow_the_document() {
    // The document must not get bigger just because it was laid out again.
    //
    // It did: box construction builds fresh anonymous blocks each pass and the
    // previous ones were only ever referenced by the list being overwritten, so
    // they stayed in the slab forever. One recorded session grew from 14 nodes
    // to 22,353 at a steady +11 per resolve, and resolve time climbed past
    // 80ms — which is what a window that grows slower the longer it is open,
    // until its controls stop answering, looks like from the inside.
    let mut doc = ScriptDocument::from_html(
        r#"
        <style>
          .row { display: flex; align-items: center; gap: 8px }
          .row::after { content: "!" }
        </style>
        <div class="row">text <b>bold</b> and <i>more</i> text</div>
        <div class="row">second <span>row</span> of content</div>
        "#,
        DocumentConfig {
            viewport: Some(Viewport::new(800, 600, 1.0, ColorScheme::Light)),
            ..DocumentConfig::default()
        },
    );
    doc.execute_scripts();

    doc.inner_mut().resolve(0.0);
    let settled = doc.inner().tree().len();

    for _ in 0..20 {
        // Damage the whole tree, as a non-incremental pass does.
        doc.inner_mut()
            .set_viewport(Viewport::new(800, 600, 1.0, ColorScheme::Light));
        doc.inner_mut().resolve(0.0);
    }

    let after = doc.inner().tree().len();
    assert_eq!(
        after,
        settled,
        "twenty resolves added {} nodes; construction is leaking anonymous boxes",
        after as i64 - settled as i64
    );
}

#[test]
fn custom_elements_define_upgrades_existing_elements() {
    // Before this, `customElements` did not exist at all, so
    // `customElements.define(...)` threw a ReferenceError out of whatever
    // module ran it. A framework that registers its components at import time
    // loses that entire module, and the page renders as unstyled markup.
    let mut doc = ScriptDocument::from_html(
        r#"<div id="host"><my-card id="a"></my-card><my-card id="b"></my-card></div>"#,
        DocumentConfig::default(),
    );
    doc.execute_scripts();

    let result = doc
        .eval_json(
            r#"
            let connected = 0;
            class MyCard extends HTMLElement {
              connectedCallback() { connected += 1; this.setAttribute("upgraded", "yes"); }
              label() { return "card:" + this.id; }
            }
            customElements.define("my-card", MyCard);

            const a = document.getElementById("a");
            ({
              // The class's methods reach the element.
              label: a.label(),
              // connectedCallback ran once per existing element.
              connected,
              // and it could touch the DOM.
              attr: a.getAttribute("upgraded"),
              // the registry answers lookups both ways
              got: customElements.get("my-card") === MyCard,
              name: customElements.getName(MyCard),
              missing: customElements.get("not-defined") === undefined,
            })
            "#,
        )
        .expect("customElements.define should evaluate");

    assert_eq!(
        result["label"].as_str(),
        Some("card:a"),
        "the class's methods must reach the element: {result}"
    );
    assert_eq!(
        result["connected"].as_i64(),
        Some(2),
        "connectedCallback must run once per existing element: {result}"
    );
    assert_eq!(
        result["attr"].as_str(),
        Some("yes"),
        "connectedCallback must be able to mutate the element: {result}"
    );
    assert_eq!(result["got"].as_bool(), Some(true), "get: {result}");
    assert_eq!(result["name"].as_str(), Some("my-card"), "getName: {result}");
    assert_eq!(result["missing"].as_bool(), Some(true), "get: {result}");
}

#[test]
fn custom_elements_rejects_a_name_without_a_dash() {
    // The dash is what keeps the custom element namespace disjoint from HTML's,
    // so a name without one is a TypeError rather than a silent no-op.
    let mut doc = ScriptDocument::from_html("<div></div>", DocumentConfig::default());
    doc.execute_scripts();

    let result = doc
        .eval_json(
            r#"
            const attempt = (name) => {
              try { customElements.define(name, class extends HTMLElement {}); return "ok"; }
              catch (e) { return e.constructor.name; }
            };
            ({ bare: attempt("card"), dashed: attempt("my-card"),
               twice: attempt("my-card") })
            "#,
        )
        .expect("should evaluate");

    assert_eq!(result["bare"].as_str(), Some("TypeError"), "{result}");
    assert_eq!(result["dashed"].as_str(), Some("ok"), "{result}");
    assert_eq!(
        result["twice"].as_str(),
        Some("TypeError"),
        "defining the same name twice must throw: {result}"
    );
}

#[test]
fn a_custom_element_created_after_define_is_upgraded_on_insertion() {
    // The other half of upgrading. Frameworks define their components at import
    // time and create the elements later, so an upgrade pass that only visited
    // what was already in the document would miss every element that matters.
    let mut doc = ScriptDocument::from_html(r#"<div id="host"></div>"#, DocumentConfig::default());
    doc.execute_scripts();

    let result = doc
        .eval_json(
            r#"
            const seen = [];
            class MyChip extends HTMLElement {
              connectedCallback() { seen.push(this.getAttribute("label")); }
            }
            customElements.define("my-chip", MyChip);

            const host = document.getElementById("host");
            const first = document.createElement("my-chip");
            first.setAttribute("label", "one");
            host.appendChild(first);

            const second = document.createElement("my-chip");
            second.setAttribute("label", "two");
            host.append(second);

            ({ seen, isInstance: first instanceof MyChip, count: host.children.length })
            "#,
        )
        .expect("should evaluate");

    assert_eq!(
        result["seen"],
        serde_json::json!(["one", "two"]),
        "connectedCallback must run on insertion, for appendChild and append: {result}"
    );
    assert_eq!(
        result["isInstance"].as_bool(),
        Some(true),
        "an upgraded element must be an instance of its class: {result}"
    );
    assert_eq!(result["count"].as_i64(), Some(2), "{result}");
}
