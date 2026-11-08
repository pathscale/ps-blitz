use blitz_dom::DocumentConfig;
use blitz_script::BoundScriptDocument;
use blitz_script::native::{Guest, GuestHost, NativeStorageBridge, Result, RuntimeBridge, Value};

struct TestBridge;
impl RuntimeBridge for TestBridge {}
impl NativeStorageBridge for TestBridge {
    fn storage_get(&mut self, _: &str) -> Result<Option<Value>> {
        Ok(None)
    }
    fn storage_set(&mut self, _: &str, _: Value) -> Result<()> {
        Ok(())
    }
    fn storage_remove(&mut self, _: &str) -> Result<()> {
        Ok(())
    }
    fn storage_clear(&mut self) -> Result<()> {
        Ok(())
    }
}

fn run(source: &str) {
    let html = concat!(
        "<!doctype html><section id='root'><div id='skip'><i id='one'>one</i>",
        "<i id='two'>two</i></div><div id='reject'><i id='pruned'>pruned</i></div>",
        "<!--comment--><i id='last'>last</i></section><i id='outside'>outside</i>",
    );
    let mut document = BoundScriptDocument::new_bound(
        html.into(),
        DocumentConfig::default(),
        GuestHost(Guest::new(TestBridge)),
    )
    .unwrap();
    document
        .eval("function check(condition, label) { if (!condition) throw new Error(label); }")
        .unwrap();
    document.eval(source).unwrap();
}

#[test]
fn walks_the_filtered_live_tree_in_both_directions() {
    run(r#"
        const root = document.getElementById("root");
        const one = document.getElementById("one");
        const two = document.getElementById("two");
        const last = document.getElementById("last");
        const filter = {
            acceptNode(node) {
                check(this === filter, "acceptNode receiver");
                check(node.nodeType === 1 && node.id !== "pruned", "mask and rejected subtree");
                if (node.id === "skip") return NodeFilter.FILTER_SKIP;
                if (node.id === "reject") return NodeFilter.FILTER_REJECT;
                return NodeFilter.FILTER_ACCEPT;
            }
        };
        const walker = document.createTreeWalker(root, NodeFilter.SHOW_ELEMENT, filter);
        check(walker instanceof TreeWalker, "interface prototype");
        check(walker.root === root && walker.currentNode === root, "initial node identity");
        check(walker.filter === filter && walker.whatToShow === NodeFilter.SHOW_ELEMENT, "options");
        check(walker.parentNode() === null && walker.previousNode() === null, "root boundary");
        check(walker.firstChild() === one, "first visible child through skipped parent");
        check(walker.nextSibling() === two, "next visible sibling");
        check(walker.nextSibling() === last, "exit skipped parent and prune rejected sibling");
        check(walker.nextSibling() === null && walker.nextNode() === null, "stay inside root");
        check(walker.currentNode === last, "failed moves preserve the current node");
        check(walker.previousSibling() === two, "reverse sibling through skipped parent");
        check(walker.previousNode() === one, "previous in tree order");
        check(walker.parentNode() === root, "visible parent can be the root");
        check(walker.lastChild() === last, "last visible child");
        walker.currentNode = root;
        check(walker.nextNode() === one && walker.nextNode() === two, "forward tree order");
        check(walker.nextNode() === last && walker.previousNode() === two, "prune in both directions");

        const added = document.createElement("i");
        root.appendChild(added);
        walker.currentNode = last;
        check(walker.nextNode() === added && walker.currentNode === added, "live append");
    "#);
}

#[test]
fn masks_nodes_before_calling_a_function_filter() {
    run(r#"
        const root = document.getElementById("root");
        const whole = document.createTreeWalker(document);
        check(whole.root === document && whole.currentNode === document, "document identity");
        check(whole.whatToShow === NodeFilter.SHOW_ALL && whole.filter === null, "defaults");

        const one = document.getElementById("one").firstChild;
        const two = document.getElementById("two").firstChild;
        const calls = [];
        const walker = document.createTreeWalker(root, NodeFilter.SHOW_TEXT, node => {
            check(node.nodeType === 3, "masked nodes never reach the filter");
            calls.push(node);
            return node === one ? NodeFilter.FILTER_SKIP : NodeFilter.FILTER_ACCEPT;
        });
        check(walker.firstChild() === two, "text descendants of masked elements");
        check(calls.length === 2 && calls[0] === one && calls[1] === two, "filter node identities");
        check(walker.parentNode() === null && walker.currentNode === two, "masked parents");
        const coerced = document.createTreeWalker(root, {
            valueOf() { return NodeFilter.SHOW_TEXT; }
        });
        check(coerced.nextNode() === one, "mask conversion uses valueOf");

        const none = document.createTreeWalker(root, 0);
        check(none.nextNode() === null && none.currentNode === root, "empty mask");
        check(document.createTreeWalker(root, -1).whatToShow === NodeFilter.SHOW_ALL, "unsigned mask");
    "#);
}

#[test]
fn recursive_filter_errors_preserve_the_cursor_and_release_the_guard() {
    run(r#"
        const root = document.getElementById("root");
        const first = document.getElementById("skip");
        let reenter = true;
        const walker = document.createTreeWalker(root, NodeFilter.SHOW_ELEMENT, () => {
            if (reenter) return walker.nextNode();
            return NodeFilter.FILTER_ACCEPT;
        });
        let errorName = "";
        try {
            walker.nextNode();
        } catch (error) {
            errorName = error.name;
        }
        check(errorName === "InvalidStateError", "reentry must propagate an error");
        check(walker.currentNode === root, "failed filter does not advance");
        reenter = false;
        check(walker.nextNode() === first, "the guard is cleared after a throw");
    "#);
}
