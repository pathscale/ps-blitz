(function () {
    "use strict";

    const create = globalThis.__docwriteFontCreate;
    const read = globalThis.__docwriteFontRead;
    const load = globalThis.__docwriteFontLoad;
    const member = globalThis.__docwriteFontMember;
    const snapshot = globalThis.__docwriteFonts;
    const query = globalThis.__docwriteFontQuery;
    const flush = globalThis.__docwriteFontFlush;
    const define = Object.defineProperty;
    const faceState = new WeakMap();
    const faces = new Map();
    const pendingFaces = new Set();
    const secret = {};

    function error(name, message) {
        if (typeof globalThis.DOMException === "function") {
            return new DOMException(message, name);
        }
        const value = new Error(message);
        value.name = name;
        return value;
    }

    function stateOf(face) {
        const state = faceState.get(face);
        if (!state) throw new TypeError("Illegal FontFace receiver");
        return state;
    }

    function initialize(face, handle) {
        const details = read(handle);
        let resolve;
        let reject;
        const loaded = new Promise(function (yes, no) {
            resolve = yes;
            reject = no;
        });
        loaded.catch(function () {});
        faceState.set(face, {
            handle,
            id: details[0],
            loaded,
            resolve,
            reject,
            settled: false
        });
        faces.set(details[0], face);
        settle(face);
    }

    function settle(face) {
        const state = stateOf(face);
        if (state.settled) return false;
        const status = read(state.handle)[4];
        if (status === "loaded") {
            state.settled = true;
            pendingFaces.delete(face);
            state.resolve(face);
            return true;
        }
        if (status === "error") {
            state.settled = true;
            pendingFaces.delete(face);
            state.reject(error("NetworkError", "Font loading failed"));
            return true;
        }
        if (status === "loading") pendingFaces.add(face);
        return false;
    }

    function wrap(handle) {
        const id = read(handle)[0];
        if (faces.has(id)) return faces.get(id);
        return new FontFace(secret, handle);
    }

    class FontFace {
        constructor(family, source, descriptors) {
            if (family === secret) {
                initialize(this, source);
                return;
            }
            if (arguments.length < 2) {
                throw new TypeError("FontFace requires a family and source");
            }
            descriptors = descriptors || {};
            if (ArrayBuffer.isView(source)) {
                source = source.buffer.slice(
                    source.byteOffset,
                    source.byteOffset + source.byteLength
                );
            }
            const handle = create(
                String(family),
                source instanceof ArrayBuffer ? source : String(source),
                descriptors.style === undefined ? "normal" : String(descriptors.style),
                descriptors.weight === undefined ? "normal" : String(descriptors.weight)
            );
            initialize(this, handle);
        }
        get family() { return read(stateOf(this).handle)[1]; }
        get style() { return read(stateOf(this).handle)[2]; }
        get weight() { return read(stateOf(this).handle)[3]; }
        get status() { return read(stateOf(this).handle)[4]; }
        get loaded() {
            settle(this);
            return stateOf(this).loaded;
        }
        load() {
            load(stateOf(this).handle);
            settle(this);
            fontSet.sync();
            return stateOf(this).loaded;
        }
    }
    define(FontFace.prototype, Symbol.toStringTag, { value: "FontFace" });
    define(globalThis, "FontFace", {
        value: FontFace,
        configurable: true,
        writable: true
    });

    class FontFaceSetLoadEvent extends Event {
        constructor(type, init) {
            init = init || {};
            super(type, init);
            Object.setPrototypeOf(this, new.target.prototype);
            define(this, "fontfaces", {
                value: Object.freeze(Array.from(init.fontfaces || [])),
                enumerable: true
            });
        }
    }
    define(FontFaceSetLoadEvent.prototype, Symbol.toStringTag, {
        value: "FontFaceSetLoadEvent"
    });
    define(globalThis, "FontFaceSetLoadEvent", {
        value: FontFaceSetLoadEvent,
        configurable: true,
        writable: true
    });

    const setState = new WeakMap();

    function setOf(set) {
        const state = setState.get(set);
        if (!state) throw new TypeError("Illegal FontFaceSet receiver");
        return state;
    }

    function capture(options) {
        return typeof options === "boolean" ? options : Boolean(options && options.capture);
    }

    class FontFaceSet {
        constructor(token) {
            if (token !== secret) throw new TypeError("Illegal constructor");
            setState.set(this, {
                members: new Map(),
                listeners: [],
                status: "loaded",
                ready: Promise.resolve(this),
                resolve: null,
                cycle: new Set()
            });
            this.onloading = null;
            this.onloadingdone = null;
            this.onloadingerror = null;
        }

        sync() {
            const state = setOf(this);
            const native = snapshot();
            const next = new Map();
            let changed = false;
            for (const handle of native[0]) {
                const face = wrap(handle);
                next.set(stateOf(face).id, face);
                changed = settle(face) || changed;
                if (face.status === "loading") state.cycle.add(face);
            }
            if (next.size !== state.members.size ||
                Array.from(next.keys()).some(id => !state.members.has(id))) {
                changed = true;
            }
            state.members = next;
            const pending = native[1] ||
                Array.from(next.values()).some(face => face.status === "loading");
            if (pending && state.status === "loaded") {
                state.status = "loading";
                state.ready = new Promise(resolve => { state.resolve = resolve; });
                this.emit("loading", []);
                changed = true;
            } else if (!pending && state.status === "loading") {
                flush();
                state.status = "loaded";
                const completed = Array.from(state.cycle);
                state.cycle.clear();
                const successful = completed.filter(face => face.status === "loaded");
                const failed = completed.filter(face => face.status === "error");
                const resolve = state.resolve;
                state.resolve = null;
                if (resolve) resolve(this);
                this.emit("loadingdone", successful);
                if (failed.length) this.emit("loadingerror", failed);
                changed = true;
            }
            return changed;
        }

        emit(type, fontfaces) {
            const event = new FontFaceSetLoadEvent(type, { fontfaces });
            globalThis.__blitzMarkTrusted(event);
            this.dispatchEvent(event);
        }

        get status() { this.sync(); return setOf(this).status; }
        get ready() { this.sync(); return setOf(this).ready; }
        get size() { this.sync(); return setOf(this).members.size; }

        add(face) {
            member(stateOf(face).handle, true);
            this.sync();
            return this;
        }

        delete(face) {
            const changed = member(stateOf(face).handle, false);
            this.sync();
            return changed;
        }

        clear() {
            this.sync();
            for (const face of setOf(this).members.values()) {
                member(stateOf(face).handle, false);
            }
            this.sync();
        }

        has(face) {
            this.sync();
            return faceState.has(face) && setOf(this).members.has(stateOf(face).id);
        }

        check(font, text) {
            if (!arguments.length) throw new TypeError("check requires a font");
            if (text !== undefined) String(text);
            return query(String(font)).map(wrap).every(face => face.status === "loaded");
        }

        load(font, text) {
            if (!arguments.length) return Promise.reject(new TypeError("load requires a font"));
            try {
                if (text !== undefined) String(text);
                const matching = query(String(font)).map(wrap);
                return Promise.all(matching.map(face => face.load()));
            } catch (failure) {
                return Promise.reject(failure);
            }
        }

        *values() {
            this.sync();
            yield* setOf(this).members.values();
        }
        keys() { return this.values(); }
        *entries() {
            for (const face of this.values()) yield [face, face];
        }
        [Symbol.iterator]() { return this.values(); }
        forEach(callback, receiver) {
            if (typeof callback !== "function") throw new TypeError("Invalid callback");
            for (const face of this.values()) callback.call(receiver, face, face, this);
        }

        addEventListener(type, callback, options) {
            const state = setOf(this);
            if (callback == null) return;
            type = String(type);
            const useCapture = capture(options);
            if (state.listeners.some(listener =>
                listener.type === type && listener.callback === callback &&
                listener.capture === useCapture
            )) return;
            const signal = options && typeof options === "object" ? options.signal : null;
            if (signal && signal.aborted) return;
            const listener = {
                type,
                callback,
                capture: useCapture,
                once: Boolean(options && typeof options === "object" && options.once),
                signal,
                abort: null
            };
            if (signal) {
                listener.abort = () => this.removeEventListener(type, callback, useCapture);
                signal.addEventListener("abort", listener.abort, { once: true });
            }
            state.listeners.push(listener);
        }

        removeEventListener(type, callback, options) {
            const state = setOf(this);
            type = String(type);
            const useCapture = capture(options);
            state.listeners = state.listeners.filter(listener => {
                const remove = listener.type === type && listener.callback === callback &&
                    listener.capture === useCapture;
                if (remove && listener.signal && listener.abort) {
                    listener.signal.removeEventListener("abort", listener.abort);
                }
                return !remove;
            });
        }

        dispatchEvent(event) {
            if (!(event instanceof Event)) throw new TypeError("dispatchEvent requires an Event");
            if (event.eventPhase !== 0) {
                throw error("InvalidStateError", "Event is already being dispatched");
            }
            const state = setOf(this);
            define(event, "target", { value: this, configurable: true });
            define(event, "currentTarget", { value: this, configurable: true });
            define(event, "eventPhase", { value: 2, configurable: true });
            try {
                for (const listener of state.listeners.slice()) {
                    if (listener.type !== event.type || !state.listeners.includes(listener)) continue;
                    if (listener.once) {
                        this.removeEventListener(listener.type, listener.callback, listener.capture);
                    }
                    try {
                        if (typeof listener.callback === "function") {
                            listener.callback.call(this, event);
                        } else if (listener.callback &&
                            typeof listener.callback.handleEvent === "function") {
                            listener.callback.handleEvent(event);
                        }
                    } catch (failure) {
                        console.error(failure);
                    }
                }
                const callback = this["on" + event.type];
                if (typeof callback === "function") {
                    try { callback.call(this, event); }
                    catch (failure) { console.error(failure); }
                }
            } finally {
                define(event, "currentTarget", { value: null, configurable: true });
                define(event, "eventPhase", { value: 0, configurable: true });
            }
            return !event.defaultPrevented;
        }
    }

    Object.setPrototypeOf(FontFaceSet.prototype, EventTarget.prototype);
    Object.setPrototypeOf(FontFaceSet, EventTarget);
    define(FontFaceSet.prototype, Symbol.toStringTag, { value: "FontFaceSet" });
    define(globalThis, "FontFaceSet", {
        value: FontFaceSet,
        configurable: true,
        writable: true
    });

    const fontSet = new FontFaceSet(secret);
    define(Document.prototype, "fonts", {
        get: function () {
            if (this !== document) throw new TypeError("Unsupported Document receiver");
            fontSet.sync();
            return fontSet;
        },
        configurable: true,
        enumerable: true
    });
    define(globalThis, "__blitzDocwritePoll", {
        value: function () {
            let changed = false;
            for (const face of Array.from(pendingFaces)) {
                changed = settle(face) || changed;
            }
            return fontSet.sync() || changed;
        },
        configurable: true
    });

    for (const name of [
        "__docwriteFontCreate", "__docwriteFontRead", "__docwriteFontLoad",
        "__docwriteFontMember", "__docwriteFonts", "__docwriteFontQuery",
        "__docwriteFontFlush"
    ]) {
        delete globalThis[name];
    }
})();
