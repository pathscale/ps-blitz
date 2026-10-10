(function (g) {
    "use strict";

    const define = Object.defineProperty;
    const Exception = g.DOMException;
    const EventCtor = g.MessageEvent;
    const dispatchWindow = g.__blitzDispatchMessageEvent;
    const schedule = g.setTimeout;
    const token = {};
    const transfers = new WeakMap();
    const itemStates = new WeakMap();
    const listStates = new WeakMap();
    const fileStates = new WeakMap();
    const portStates = new WeakMap();

    function publish(name, constructor) {
        define(constructor.prototype, Symbol.toStringTag, {
            value: name, configurable: true
        });
        define(g, name, { value: constructor, writable: true, configurable: true });
    }

    function state(map, object) {
        const value = map.get(object);
        if (!value) throw new TypeError("Illegal invocation");
        return value;
    }

    function domString(value) {
        if (typeof value === "symbol") throw new TypeError("Cannot convert a Symbol to a string");
        return String(value);
    }

    function asciiLower(value) {
        return domString(value).replace(/[A-Z]/g, character => character.toLowerCase());
    }

    function format(value) {
        const name = asciiLower(value);
        return name === "text" ? "text/plain" : name === "url" ? "text/uri-list" : name;
    }

    function index(key) {
        if (typeof key !== "string") return null;
        const value = Number(key);
        return Number.isInteger(value) && value >= 0 && value < 4294967295 &&
            String(value) === key ? value : null;
    }

    function indexed(target, read, map, owner) {
        const proxy = new Proxy(target, {
            get(object, key, receiver) {
                const number = index(key);
                return number === null ? Reflect.get(object, key, receiver) : read()[number];
            },
            has(object, key) {
                const number = index(key);
                return number === null ? Reflect.has(object, key) : number < read().length;
            },
            ownKeys(object) {
                return read().map((_, number) => String(number)).concat(
                    Reflect.ownKeys(object).filter(key => index(key) === null)
                );
            },
            getOwnPropertyDescriptor(object, key) {
                const number = index(key);
                if (number === null) return Reflect.getOwnPropertyDescriptor(object, key);
                if (number >= read().length) return undefined;
                return {
                    value: read()[number], writable: false,
                    enumerable: true, configurable: true
                };
            },
            set(object, key, value, receiver) {
                return index(key) === null && Reflect.set(object, key, value, receiver);
            },
            deleteProperty(object, key) {
                return index(key) === null && Reflect.deleteProperty(object, key);
            },
            preventExtensions() { return false; }
        });
        map.set(proxy, owner);
        return proxy;
    }

    class DataTransferItem {
        constructor(secret, kind, type, data) {
            if (secret !== token) throw new TypeError("Illegal constructor");
            itemStates.set(this, { kind, type, data });
        }
        get kind() { return state(itemStates, this).kind; }
        get type() { return state(itemStates, this).type; }
        getAsFile() {
            const item = state(itemStates, this);
            return item.kind === "file" ? item.data : null;
        }
        getAsString(callback) {
            const item = state(itemStates, this);
            if (callback == null) return;
            if (typeof callback !== "function") throw new TypeError("Callback must be callable");
            if (item.kind === "string") schedule(() => callback(item.data), 0);
        }
    }

    class DataTransferItemList {
        constructor(secret, owner) {
            if (secret !== token) throw new TypeError("Illegal constructor");
            listStates.set(this, owner);
            return indexed(this, () => owner.entries, listStates, owner);
        }
        get length() { return state(listStates, this).entries.length; }
        add(data, type) {
            const owner = state(listStates, this);
            if (!arguments.length) throw new TypeError("Data is required");
            const isFile = typeof g.File === "function" && data instanceof g.File;
            let item;
            if (arguments.length === 1 && isFile) {
                item = new DataTransferItem(token, "file", asciiLower(data.type), data);
            } else {
                if (arguments.length < 2) throw new TypeError("A string item requires a type");
                data = domString(data);
                type = asciiLower(type);
                if (owner.entries.some(entry => entry.kind === "string" && entry.type === type)) {
                    throw new Exception("A string item with this type already exists", "NotSupportedError");
                }
                item = new DataTransferItem(token, "string", type, data);
            }
            owner.entries.push(item);
            return item;
        }
        remove(number) {
            const entries = state(listStates, this).entries;
            number = Number(number) >>> 0;
            if (number < entries.length) entries.splice(number, 1);
        }
        clear() { state(listStates, this).entries.length = 0; }
        *[Symbol.iterator]() {
            const owner = state(listStates, this);
            for (let number = 0; number < owner.entries.length; number++) {
                yield owner.entries[number];
            }
        }
    }

    class FileList {
        constructor(secret, owner) {
            if (secret !== token) throw new TypeError("Illegal constructor");
            fileStates.set(this, owner);
            return indexed(this, () => owner.entries
                .filter(item => item.kind === "file").map(item => item.getAsFile()),
                fileStates, owner);
        }
        get length() {
            return state(fileStates, this).entries.filter(item => item.kind === "file").length;
        }
        item(number) {
            return state(fileStates, this).entries.filter(item => item.kind === "file")
                .map(item => item.getAsFile())[Number(number) >>> 0] || null;
        }
        *[Symbol.iterator]() {
            for (let number = 0; number < this.length; number++) yield this.item(number);
        }
    }

    class DataTransfer {
        constructor() {
            const owner = { entries: [], dropEffect: "none", effectAllowed: "none" };
            owner.items = new DataTransferItemList(token, owner);
            owner.files = new FileList(token, owner);
            transfers.set(this, owner);
        }
        get items() { return state(transfers, this).items; }
        get files() { return state(transfers, this).files; }
        get types() {
            const entries = state(transfers, this).entries;
            const types = entries.filter(item => item.kind === "string").map(item => item.type);
            if (entries.some(item => item.kind === "file")) types.push("Files");
            return Object.freeze(types);
        }
        get dropEffect() { return state(transfers, this).dropEffect; }
        set dropEffect(value) {
            const owner = state(transfers, this);
            value = domString(value);
            if (["none", "copy", "link", "move"].includes(value)) owner.dropEffect = value;
        }
        get effectAllowed() { return state(transfers, this).effectAllowed; }
        set effectAllowed(value) {
            const owner = state(transfers, this);
            value = domString(value);
            if (["none", "copy", "copyLink", "copyMove", "link", "linkMove",
                "move", "all", "uninitialized"].includes(value)) owner.effectAllowed = value;
        }
        setData(type, data) {
            const owner = state(transfers, this);
            if (arguments.length < 2) throw new TypeError("Type and data are required");
            type = format(type);
            data = domString(data);
            const previous = owner.entries.findIndex(item => item.kind === "string" && item.type === type);
            const item = new DataTransferItem(token, "string", type, data);
            if (previous < 0) owner.entries.push(item);
            else owner.entries[previous] = item;
        }
        getData(type) {
            const owner = state(transfers, this);
            if (!arguments.length) throw new TypeError("Type is required");
            const original = asciiLower(type);
            type = format(original);
            const item = owner.entries.find(entry => entry.kind === "string" && entry.type === type);
            if (!item) return "";
            const data = state(itemStates, item).data;
            if (original !== "url") return data;
            return data.split(/\r\n|\r|\n/).find(line => line && !line.startsWith("#")) || "";
        }
        clearData(type) {
            const owner = state(transfers, this);
            const all = type === undefined;
            const name = all ? "" : format(type);
            for (let number = owner.entries.length - 1; number >= 0; number--) {
                const item = owner.entries[number];
                if (item.kind === "string" && (all || item.type === name)) {
                    owner.entries.splice(number, 1);
                }
            }
        }
        setDragImage(element, x, y) {
            state(transfers, this);
            if (arguments.length < 3 || !(element instanceof g.Element)) {
                throw new TypeError("setDragImage requires an Element and coordinates");
            }
            Number(x);
            Number(y);
        }
    }

    function portState(port) { return state(portStates, port); }

    function pump(port) {
        const value = portState(port);
        if (!value.started || value.closed || value.scheduled || !value.queue.length) return;
        value.scheduled = true;
        schedule(function () {
            value.scheduled = false;
            if (!value.started || value.closed || !value.queue.length) return;
            const data = value.queue.shift();
            try {
                port.dispatchEvent(new EventCtor("message", { data }));
            } finally {
                pump(port);
            }
        }, 0);
    }

    function handler(port, type, starts) {
        let value = null;
        let installed = false;
        define(port, "on" + type, {
            enumerable: true,
            configurable: true,
            get() { return value; },
            set(next) {
                value = typeof next === "function" ? next : null;
                if (value && !installed) {
                    installed = true;
                    port.addEventListener(type, event => {
                        if (value) value.call(port, event);
                    });
                }
                if (starts) port.start();
            }
        });
    }

    class MessagePort extends g.EventTarget {
        constructor(secret) {
            if (secret !== token) throw new TypeError("Illegal constructor");
            super();
            portStates.set(this, {
                peer: null, queue: [], started: false, closed: false, scheduled: false
            });
            handler(this, "message", true);
            handler(this, "messageerror", false);
        }
        postMessage(message, options) {
            const value = portState(this);
            if (!arguments.length) throw new TypeError("Message is required");
            const transfer = Array.from(Array.isArray(options) ? options :
                options && options.transfer || []);
            if (transfer.some(entry => portStates.has(entry))) {
                throw new Exception("MessagePort transfer is not supported", "DataCloneError");
            }
            const data = g.structuredClone(message, { transfer });
            if (value.closed || !value.peer) return;
            const peer = portState(value.peer);
            if (peer.closed) return;
            peer.queue.push(data);
            pump(value.peer);
        }
        start() {
            const value = portState(this);
            if (value.closed) return;
            value.started = true;
            pump(this);
        }
        close() {
            const value = portState(this);
            value.closed = true;
            value.queue.length = 0;
            if (value.peer) portState(value.peer).peer = null;
            value.peer = null;
        }
    }

    class MessageChannel {
        constructor() {
            const port1 = new MessagePort(token);
            const port2 = new MessagePort(token);
            portState(port1).peer = port2;
            portState(port2).peer = port1;
            define(this, "port1", { value: port1, enumerable: true });
            define(this, "port2", { value: port2, enumerable: true });
        }
    }

    for (const [name, constructor] of [
        ["DataTransferItem", DataTransferItem], ["DataTransferItemList", DataTransferItemList],
        ["FileList", FileList], ["DataTransfer", DataTransfer],
        ["MessagePort", MessagePort], ["MessageChannel", MessageChannel]
    ]) publish(name, constructor);

    const mediaProto = g.MediaQueryList.prototype;
    const dispatchMedia = mediaProto.dispatchEvent;
    const mediaMatches = Object.getOwnPropertyDescriptor(mediaProto, "matches").get;
    define(mediaProto, "dispatchEvent", {
        configurable: true,
        writable: true,
        value: function (event) {
            mediaMatches.call(this);
            if (!(event instanceof g.Event)) throw new TypeError("dispatchEvent requires an Event");
            if (!event.type || event.eventPhase !== 0) {
                throw new Exception("Event is uninitialized or already being dispatched", "InvalidStateError");
            }
            return dispatchMedia.call(this, event);
        }
    });

    define(g, "postMessage", {
        configurable: true,
        writable: true,
        value: function (message, targetOrigin, transfer) {
            if (!arguments.length) throw new TypeError("Message is required");
            if (targetOrigin && typeof targetOrigin === "object") {
                const options = targetOrigin;
                targetOrigin = options.targetOrigin;
                transfer = options.transfer;
            }
            targetOrigin = targetOrigin === undefined ? "/" : domString(targetOrigin);
            let expected = targetOrigin;
            if (targetOrigin !== "*" && targetOrigin !== "/") {
                try { expected = new g.URL(targetOrigin).origin; }
                catch (error) { throw new Exception("Invalid target origin", "SyntaxError"); }
            }
            transfer = Array.from(transfer || []);
            if (transfer.some(entry => portStates.has(entry))) {
                throw new Exception("MessagePort transfer is not supported", "DataCloneError");
            }
            const data = g.structuredClone(message, { transfer });
            const origin = new g.URL(g.location.href).origin;
            if (expected === "/") expected = origin;
            schedule(function () {
                if (expected !== "*" && expected !== new g.URL(g.location.href).origin) return;
                const event = new EventCtor("message", { data, origin, source: g });
                g.__blitzMarkTrusted(event);
                dispatchWindow(event);
            }, 0);
        }
    });
    delete g.__blitzDispatchMessageEvent;
})(globalThis);

