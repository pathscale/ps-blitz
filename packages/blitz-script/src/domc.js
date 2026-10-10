(function () {
    "use strict";

    const supports = globalThis.__domcSupports;
    const parseMedia = globalThis.__domcMedia;
    const matchesMedia = globalThis.__domcMatches;
    const collect = globalThis.__domcCollection;
    const sheetOwners = globalThis.__domcSheetOwners;
    const sheetHandle = globalThis.__domcSheetHandle;
    const sheetRead = globalThis.__domcSheetRead;
    const sheetWrite = globalThis.__domcSheetWrite;
    const immediateStopped = globalThis.__domcImmediateStopped;
    const mainDocument = globalThis.document;
    const documentProto = Object.getPrototypeOf(mainDocument);
    const define = Object.defineProperty;

    function interfacePrototype(name) {
        if (typeof globalThis[name] === "function") {
            return globalThis[name].prototype;
        }
        const constructor = function () {
            throw new TypeError("Illegal constructor");
        };
        define(constructor, "name", { value: name });
        define(constructor.prototype, Symbol.toStringTag, { value: name });
        define(globalThis, name, {
            value: constructor,
            configurable: true,
            writable: true
        });
        return constructor.prototype;
    }

    const declarationProto = globalThis.CSSStyleDeclaration.prototype;
    define(declarationProto, Symbol.toStringTag, {
        value: "CSSStyleDeclaration",
        configurable: true
    });
    define(declarationProto, Symbol.iterator, {
        value: function* () {
            for (let index = 0; index < this.length; index++) {
                yield this.item(index);
            }
        },
        configurable: true,
        writable: true
    });

    const CSS = {};
    define(CSS, Symbol.toStringTag, { value: "CSS" });
    CSS.supports = function (property, value) {
        if (!arguments.length) {
            throw new TypeError("CSS.supports requires an argument");
        }
        return arguments.length > 1
            ? supports(String(property), String(value))
            : supports(String(property));
    };
    CSS.escape = function (value) {
        if (!arguments.length) {
            throw new TypeError("CSS.escape requires an argument");
        }
        const text = String(value);
        let escaped = "";
        for (let index = 0; index < text.length; index++) {
            const code = text.charCodeAt(index);
            const character = text.charAt(index);
            if (code === 0) {
                escaped += "\uFFFD";
            } else if (
                (code >= 1 && code <= 31) || code === 127 ||
                (index === 0 && code >= 48 && code <= 57) ||
                (index === 1 && code >= 48 && code <= 57 && text.charAt(0) === "-")
            ) {
                escaped += "\\" + code.toString(16) + " ";
            } else if (index === 0 && character === "-" && text.length === 1) {
                escaped += "\\-";
            } else if (
                code >= 128 || character === "-" || character === "_" ||
                (code >= 48 && code <= 57) ||
                (code >= 65 && code <= 90) || (code >= 97 && code <= 122)
            ) {
                escaped += character;
            } else {
                escaped += "\\" + character;
            }
        }
        return escaped;
    };
    define(globalThis, "CSS", { value: CSS, configurable: true, writable: true });

    function numericIndex(key) {
        if (typeof key !== "string" || key === "") return null;
        const value = Number(key);
        return Number.isInteger(value) && value >= 0 && value < 4294967295 &&
            String(value) === key ? value : null;
    }

    function namedItem(values, name) {
        name = String(name);
        if (!name) return null;
        for (const value of values) {
            if (value.id === name) return value;
        }
        for (const value of values) {
            if (value.getAttribute("name") === name) return value;
        }
        return null;
    }

    function liveList(prototype, read, named, tag) {
        const target = Object.create(prototype);
        define(target, Symbol.toStringTag, { value: tag, configurable: true });
        define(target, "length", { get: () => read().length, configurable: true });
        define(target, "item", {
            value: function (index) { return read()[Number(index) >>> 0] || null; },
            configurable: true,
            writable: true
        });
        if (named) {
            define(target, "namedItem", {
                value: function (name) { return namedItem(read(), name); },
                configurable: true,
                writable: true
            });
        }
        define(target, Symbol.iterator, {
            value: function* () {
                for (let index = 0; index < read().length; index++) {
                    yield read()[index];
                }
            },
            configurable: true,
            writable: true
        });
        if (tag === "NodeList") {
            define(target, "values", { value: target[Symbol.iterator], configurable: true });
            define(target, "keys", {
                value: function* () {
                    for (let index = 0; index < read().length; index++) yield index;
                },
                configurable: true
            });
            define(target, "entries", {
                value: function* () {
                    for (let index = 0; index < read().length; index++) {
                        yield [index, read()[index]];
                    }
                },
                configurable: true
            });
            define(target, "forEach", {
                value: function (callback, receiver) {
                    if (typeof callback !== "function") throw new TypeError("Invalid callback");
                    const length = read().length;
                    for (let index = 0; index < length; index++) {
                        const values = read();
                        if (index < values.length) callback.call(receiver, values[index], index, proxy);
                    }
                },
                configurable: true
            });
        }
        const proxy = new Proxy(target, {
            get(object, key, receiver) {
                if (Reflect.has(object, key)) return Reflect.get(object, key, receiver);
                const index = numericIndex(key);
                if (index !== null) return read()[index];
                if (named && typeof key === "string") {
                    return namedItem(read(), key) || undefined;
                }
                return undefined;
            },
            has(object, key) {
                if (Reflect.has(object, key)) return true;
                const index = numericIndex(key);
                if (index !== null) return index < read().length;
                return named && typeof key === "string" && namedItem(read(), key) !== null;
            },
            ownKeys(object) {
                const keys = Reflect.ownKeys(object);
                const indices = read().map((_, index) => String(index));
                return indices.concat(keys.filter(key => numericIndex(key) === null));
            },
            getOwnPropertyDescriptor(object, key) {
                const descriptor = Reflect.getOwnPropertyDescriptor(object, key);
                if (descriptor) return descriptor;
                const index = numericIndex(key);
                const value = index !== null ? read()[index] :
                    named && typeof key === "string" ? namedItem(read(), key) : null;
                if (value == null) return undefined;
                return {
                    value,
                    writable: false,
                    enumerable: index !== null,
                    configurable: true
                };
            },
            set(object, key, value, receiver) {
                if (numericIndex(key) !== null) return false;
                return Reflect.set(object, key, value, receiver);
            },
            preventExtensions() { return false; }
        });
        return proxy;
    }

    const htmlCollectionProto = interfacePrototype("HTMLCollection");
    const nodeListProto = interfacePrototype("NodeList");
    const documentCollections = new WeakMap();
    function documentCollection(document, kind) {
        let collections = documentCollections.get(document);
        if (!collections) {
            collections = new Map();
            documentCollections.set(document, collections);
        }
        if (!collections.has(kind)) {
            collections.set(kind, liveList(
                htmlCollectionProto,
                () => collect(document, kind, ""),
                true,
                "HTMLCollection"
            ));
        }
        return collections.get(kind);
    }
    for (const kind of ["forms", "images", "links", "scripts", "embeds"]) {
        define(documentProto, kind, {
            get: function () { return documentCollection(this, kind); },
            configurable: true,
            enumerable: true
        });
    }
    define(documentProto, "plugins", {
        get: function () { return documentCollection(this, "embeds"); },
        configurable: true,
        enumerable: true
    });
    define(documentProto, "getElementsByName", {
        value: function (name) {
            if (!arguments.length) throw new TypeError("getElementsByName requires a name");
            name = String(name);
            const document = this;
            return liveList(nodeListProto, () => collect(document, "name", name), false, "NodeList");
        },
        configurable: true,
        writable: true
    });
    define(documentProto, "location", {
        get: function () { return this === mainDocument ? globalThis.location : null; },
        set: function (value) {
            if (this === mainDocument) globalThis.location.href = String(value);
        },
        configurable: true,
        enumerable: true
    });

    const sheetProto = interfacePrototype("StyleSheet");
    const sheetListProto = interfacePrototype("StyleSheetList");
    const mediaListProto = interfacePrototype("MediaList");
    const ruleListProto = interfacePrototype("CSSRuleList");
    const sheets = new WeakMap();
    const sheetLists = new WeakMap();

    function wrapSheet(owner) {
        if (sheets.has(owner)) return sheets.get(owner);
        const handle = sheetHandle(owner);
        if (handle === null) return null;
        const sheet = Object.create(sheetProto);
        define(sheet, "ownerNode", { get: () => owner, configurable: true });
        define(sheet, "href", { get: () => sheetRead(handle, "href"), configurable: true });
        define(sheet, "type", { value: "text/css", configurable: true });
        define(sheet, "parentStyleSheet", { value: null, configurable: true });
        define(sheet, "disabled", {
            get: () => sheetRead(handle, "disabled"),
            set: value => sheetWrite(handle, "disabled", Boolean(value)),
            configurable: true
        });

        const media = liveList(
            mediaListProto,
            () => sheetRead(handle, "mediaItems"),
            false,
            "MediaList"
        );
        define(media, "mediaText", {
            get: () => sheetRead(handle, "media"),
            set: value => sheetWrite(handle, "media", String(value)),
            configurable: true
        });
        define(media, "appendMedium", {
            value: value => sheetWrite(handle, "appendMedium", String(value)),
            configurable: true
        });
        define(media, "deleteMedium", {
            value: value => sheetWrite(handle, "deleteMedium", String(value)),
            configurable: true
        });
        define(sheet, "media", { get: () => media, configurable: true });

        const rules = liveList(
            ruleListProto,
            () => sheetRead(handle, "rules").map(text => {
                const rule = {};
                define(rule, "cssText", { value: text, enumerable: true });
                define(rule, "parentStyleSheet", { value: sheet });
                define(rule, "parentRule", { value: null });
                return rule;
            }),
            false,
            "CSSRuleList"
        );
        define(sheet, "cssRules", {
            get: () => {
                sheetRead(handle, "rules");
                return rules;
            },
            configurable: true
        });
        sheets.set(owner, sheet);
        return sheet;
    }
    define(documentProto, "styleSheets", {
        get: function () {
            if (!sheetLists.has(this)) {
                const document = this;
                sheetLists.set(document, liveList(
                    sheetListProto,
                    () => sheetOwners(document).map(wrapSheet).filter(Boolean),
                    false,
                    "StyleSheetList"
                ));
            }
            return sheetLists.get(this);
        },
        configurable: true,
        enumerable: true
    });

    const mediaState = new WeakMap();
    const watched = [];
    const active = new Set();
    const token = {};
    function stateOf(target) {
        const state = mediaState.get(target);
        if (!state) throw new TypeError("Illegal MediaQueryList receiver");
        return state;
    }
    function updateRetention(target) {
        const state = stateOf(target);
        if (state.onchange || state.listeners.some(listener => listener.type === "change")) {
            active.add(target);
        } else {
            active.delete(target);
        }
    }
    function listenerCapture(options) {
        return typeof options === "boolean" ? options : Boolean(options && options.capture);
    }
    function listenerError(error) {
        if (typeof globalThis.reportError === "function") globalThis.reportError(error);
        else console.error(error);
    }

    class MediaQueryList {
        constructor(secret, parsed) {
            if (secret !== token) throw new TypeError("Illegal constructor");
            mediaState.set(this, {
                handle: parsed[0],
                media: parsed[1],
                previous: parsed[2],
                onchange: null,
                listeners: []
            });
            watched.push(new WeakRef(this));
        }
        get media() { return stateOf(this).media; }
        get matches() { return matchesMedia(stateOf(this).handle); }
        get onchange() { return stateOf(this).onchange; }
        set onchange(value) {
            stateOf(this).onchange = typeof value === "function" ? value : null;
            updateRetention(this);
        }
        addEventListener(type, callback, options) {
            const state = stateOf(this);
            type = String(type);
            if (callback == null) return;
            const capture = listenerCapture(options);
            if (state.listeners.some(listener =>
                listener.type === type && listener.callback === callback &&
                listener.capture === capture
            )) return;
            const signal = options && typeof options === "object" ? options.signal : null;
            if (signal && signal.aborted) return;
            const listener = {
                type,
                callback,
                capture,
                once: Boolean(options && typeof options === "object" && options.once),
                signal,
                abort: null
            };
            if (signal) {
                listener.abort = () => this.removeEventListener(type, callback, capture);
                signal.addEventListener("abort", listener.abort, { once: true });
            }
            state.listeners.push(listener);
            updateRetention(this);
        }
        removeEventListener(type, callback, options) {
            const state = stateOf(this);
            type = String(type);
            const capture = listenerCapture(options);
            state.listeners = state.listeners.filter(listener => {
                const remove = listener.type === type && listener.callback === callback &&
                    listener.capture === capture;
                if (remove && listener.signal && listener.abort) {
                    listener.signal.removeEventListener("abort", listener.abort);
                }
                return !remove;
            });
            updateRetention(this);
        }
        addListener(callback) { this.addEventListener("change", callback); }
        removeListener(callback) { this.removeEventListener("change", callback); }
        dispatchEvent(event) {
            const state = stateOf(this);
            if (!(event instanceof Event)) throw new TypeError("dispatchEvent requires an Event");
            if (event.eventPhase !== 0) throw new Error("Event is already being dispatched");
            define(event, "target", { value: this, configurable: true });
            define(event, "currentTarget", { value: this, configurable: true });
            define(event, "eventPhase", { value: 2, configurable: true });
            try {
                const listeners = state.listeners.slice();
                for (const listener of listeners) {
                    if (listener.type !== event.type || !state.listeners.includes(listener)) continue;
                    if (listener.once) {
                        this.removeEventListener(listener.type, listener.callback, listener.capture);
                    }
                    try {
                        if (typeof listener.callback === "function") {
                            listener.callback.call(this, event);
                        } else if (typeof listener.callback.handleEvent === "function") {
                            listener.callback.handleEvent(event);
                        }
                    } catch (error) {
                        listenerError(error);
                    }
                    if (immediateStopped(event)) break;
                }
                if (event.type === "change" && state.onchange && !immediateStopped(event)) {
                    try { state.onchange.call(this, event); }
                    catch (error) { listenerError(error); }
                }
            } finally {
                define(event, "currentTarget", { value: null, configurable: true });
                define(event, "eventPhase", { value: 0, configurable: true });
            }
            return !event.defaultPrevented;
        }
    }
    if (typeof globalThis.EventTarget === "function") {
        Object.setPrototypeOf(MediaQueryList.prototype, globalThis.EventTarget.prototype);
        Object.setPrototypeOf(MediaQueryList, globalThis.EventTarget);
    }
    define(MediaQueryList.prototype, Symbol.toStringTag, { value: "MediaQueryList" });
    define(globalThis, "MediaQueryList", {
        value: MediaQueryList,
        configurable: true,
        writable: true
    });

    class MediaQueryListEvent extends Event {
        constructor(type, init) {
            init = init || {};
            super(type, init);
            Object.setPrototypeOf(this, new.target.prototype);
            define(this, "matches", { value: Boolean(init.matches), enumerable: true });
            define(this, "media", {
                value: init.media === undefined ? "" : String(init.media),
                enumerable: true
            });
        }
    }
    define(MediaQueryListEvent.prototype, Symbol.toStringTag, { value: "MediaQueryListEvent" });
    define(globalThis, "MediaQueryListEvent", {
        value: MediaQueryListEvent,
        configurable: true,
        writable: true
    });
    define(globalThis, "matchMedia", {
        value: function (query) {
            if (!arguments.length) throw new TypeError("matchMedia requires a query");
            return new MediaQueryList(token, parseMedia(String(query)));
        },
        configurable: true,
        writable: true
    });
    define(globalThis, "__blitzDOMCPoll", {
        value: function () {
            const changes = [];
            let write = 0;
            for (let index = 0; index < watched.length; index++) {
                const weak = watched[index];
                const target = weak.deref();
                if (!target) continue;
                watched[write++] = weak;
                const state = stateOf(target);
                const matches = matchesMedia(state.handle);
                if (matches !== state.previous) {
                    state.previous = matches;
                    changes.push([target, matches, state.media]);
                }
            }
            watched.length = write;
            for (const [target, matches, media] of changes) {
                const event = new MediaQueryListEvent("change", { matches, media });
                globalThis.__blitzMarkTrusted(event);
                target.dispatchEvent(event);
            }
            return changes.length > 0;
        },
        configurable: true
    });

    for (const name of [
        "__domcSupports", "__domcMedia", "__domcMatches", "__domcCollection",
        "__domcSheetOwners", "__domcSheetHandle", "__domcSheetRead", "__domcSheetWrite",
        "__domcImmediateStopped"
    ]) {
        delete globalThis[name];
    }
})();
