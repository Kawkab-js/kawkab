(function () {
  var g = typeof globalThis !== "undefined" ? globalThis : typeof global !== "undefined" ? global : this;

  function utf8EncodeManual(str) {
    str = String(str);
    var bytes = [];
    for (var i = 0; i < str.length; i++) {
      var c = str.charCodeAt(i);
      if (c < 0x80) bytes.push(c);
      else if (c < 0x800) {
        bytes.push(0xc0 | (c >> 6), 0x80 | (c & 0x3f));
      } else if (c >= 0xd800 && c <= 0xdbff && i + 1 < str.length) {
        var d = str.charCodeAt(++i);
        if (d >= 0xdc00 && d <= 0xdfff) {
          c = 0x10000 + ((c & 0x3ff) << 10) + (d & 0x3ff);
          bytes.push(
            0xf0 | (c >> 18),
            0x80 | ((c >> 12) & 0x3f),
            0x80 | ((c >> 6) & 0x3f),
            0x80 | (c & 0x3f)
          );
        } else {
          bytes.push(0xef, 0xbf, 0xbd);
        }
      } else {
        bytes.push(0xe0 | (c >> 12), 0x80 | ((c >> 6) & 0x3f), 0x80 | (c & 0x3f));
      }
    }
    return new Uint8Array(bytes);
  }

  function utf8Encode(str) {
    str = String(str);
    if (typeof TextEncoder !== "undefined") return new TextEncoder().encode(str);
    return utf8EncodeManual(str);
  }

  function utf8DecodeManual(bytes) {
    var s = "";
    for (var i = 0; i < bytes.length; i++) s += String.fromCharCode(bytes[i]);
    try {
      return decodeURIComponent(escape(s));
    } catch (e) {
      return s;
    }
  }

  function utf8Decode(bytes) {
    if (typeof TextDecoder !== "undefined") return new TextDecoder("utf-8").decode(bytes);
    return utf8DecodeManual(bytes);
  }

  function DOMException(message, name) {
    if (!(this instanceof DOMException)) return new DOMException(message, name);
    var msg = message != null ? String(message) : "";
    var n = name != null ? String(name) : "Error";
    Error.call(this, msg);
    this.name = n;
    this.message = msg;
    this.code = 0;
  }
  DOMException.prototype = Object.create(Error.prototype);
  DOMException.prototype.constructor = DOMException;

  function Event(type, init) {
    if (!(this instanceof Event)) return new Event(type, init);
    this.type = String(type || "");
    init = init || {};
    this.bubbles = !!init.bubbles;
    this.cancelable = !!init.cancelable;
    this.composed = !!init.composed;
    this.defaultPrevented = false;
    this.timeStamp = typeof performance !== "undefined" && performance.now ? performance.now() : Date.now();
    this._stop = false;
    this._stopImmediate = false;
  }
  Event.prototype.preventDefault = function () {
    if (this.cancelable) this.defaultPrevented = true;
  };
  Event.prototype.stopPropagation = function () {
    this._stop = true;
  };
  Event.prototype.stopImmediatePropagation = function () {
    this._stopImmediate = true;
    this._stop = true;
  };

  function EventTarget() {
    this._listeners = {};
  }
  EventTarget.prototype.addEventListener = function (type, fn, opts) {
    if (typeof fn !== "function") return;
    type = String(type);
    var capture = opts === true || (opts && opts.capture);
    var key = type + (capture ? ":capture" : ":bubble");
    if (!this._listeners[key]) this._listeners[key] = [];
    this._listeners[key].push(fn);
  };
  EventTarget.prototype.removeEventListener = function (type, fn, opts) {
    type = String(type);
    var capture = opts === true || (opts && opts.capture);
    var key = type + (capture ? ":capture" : ":bubble");
    var list = this._listeners[key];
    if (!list) return;
    for (var i = 0; i < list.length; i++) {
      if (list[i] === fn) {
        list.splice(i, 1);
        break;
      }
    }
  };
  EventTarget.prototype.dispatchEvent = function (ev) {
    var type = ev.type;
    var keys = [type + ":capture", type + ":bubble"];
    for (var k = 0; k < keys.length; k++) {
      var list = this._listeners[keys[k]] || [];
      var copy = list.slice();
      for (var i = 0; i < copy.length; i++) {
        if (ev._stopImmediate) break;
        try {
          copy[i].call(this, ev);
        } catch (e) {}
        if (ev._stopImmediate) break;
      }
      if (ev._stop) break;
    }
    return !ev.defaultPrevented;
  };

  function CustomEvent(type, init) {
    if (!(this instanceof CustomEvent)) return new CustomEvent(type, init);
    Event.call(this, type, init);
    this.detail = init && init.detail !== undefined ? init.detail : null;
  }
  CustomEvent.prototype = Object.create(Event.prototype);
  CustomEvent.prototype.constructor = CustomEvent;

  function MessageEvent(type, init) {
    if (!(this instanceof MessageEvent)) return new MessageEvent(type, init);
    Event.call(this, type, init);
    init = init || {};
    this.data = init.data;
    this.origin = init.origin != null ? String(init.origin) : "";
    this.lastEventId = init.lastEventId != null ? String(init.lastEventId) : "";
  }
  MessageEvent.prototype = Object.create(Event.prototype);
  MessageEvent.prototype.constructor = MessageEvent;

  function materializeBlobParts(parts, out) {
    for (var i = 0; i < parts.length; i++) {
      var p = parts[i];
      if (p instanceof Blob) {
        materializeBlobParts(p._buffers, out);
      } else if (typeof p === "string") {
        out.push(utf8Encode(p));
      } else if (p && typeof p.byteLength === "number") {
        var u = p instanceof Uint8Array ? p : new Uint8Array(p.buffer, p.byteOffset || 0, p.byteLength);
        if (u.byteLength) out.push(new Uint8Array(u));
      }
    }
  }

  function Blob(parts, options) {
    if (!(this instanceof Blob)) return new Blob(parts, options);
    options = options || {};
    var t = options.type != null ? String(options.type) : "";
    this.type = t.split(";")[0].trim().toLowerCase();
    this._buffers = [];
    parts = parts == null ? [] : parts;
    for (var i = 0; i < parts.length; i++) {
      var p = parts[i];
      if (p instanceof Blob) this._buffers.push(p);
      else if (typeof p === "string") this._buffers.push(utf8Encode(p));
      else if (p && typeof p.byteLength === "number") {
        var u = p instanceof Uint8Array ? p : new Uint8Array(p.buffer, p.byteOffset || 0, p.byteLength);
        if (u.byteLength) this._buffers.push(new Uint8Array(u));
      }
    }
    this.size = 0;
    for (var j = 0; j < this._buffers.length; j++) {
      this.size += this._buffers[j] instanceof Blob ? this._buffers[j].size : this._buffers[j].length;
    }
  }
  Blob.prototype.slice = function (start, end, contentType) {
    var flat = [];
    materializeBlobParts(this._buffers, flat);
    var total = 0;
    for (var i = 0; i < flat.length; i++) total += flat[i].length;
    var s = start == null ? 0 : Math.max(0, Math.floor(start));
    var e = end == null ? total : Math.min(total, Math.max(0, Math.floor(end)));
    if (e < s) e = s;
    var out = new Uint8Array(e - s);
    var off = 0;
    var skip = s;
    for (var j = 0; j < flat.length && off < out.length; j++) {
      var chunk = flat[j];
      if (skip >= chunk.length) {
        skip -= chunk.length;
        continue;
      }
      var take = Math.min(chunk.length - skip, out.length - off);
      out.set(chunk.subarray(skip, skip + take), off);
      off += take;
      skip = 0;
    }
    var ct = contentType != null ? String(contentType).split(";")[0].trim().toLowerCase() : this.type;
    return new Blob([out], { type: ct });
  };
  Blob.prototype.arrayBuffer = function () {
    var flat = [];
    materializeBlobParts(this._buffers, flat);
    var total = 0;
    for (var i = 0; i < flat.length; i++) total += flat[i].length;
    var merged = new Uint8Array(total);
    var o = 0;
    for (var j = 0; j < flat.length; j++) {
      merged.set(flat[j], o);
      o += flat[j].length;
    }
    return Promise.resolve(merged.buffer);
  };
  Blob.prototype.text = function () {
    var self = this;
    return self.arrayBuffer().then(function (buf) {
      return utf8Decode(new Uint8Array(buf));
    });
  };

  function formValueToString(v) {
    if (v == null) return "";
    if (typeof v === "string") return v;
    if (typeof Blob !== "undefined" && v instanceof Blob) return "[object Blob]";
    return String(v);
  }

  function FormData() {
    this._entries = [];
  }
  FormData.prototype.append = function (name, value, filename) {
    this._entries.push({ name: String(name), value: value, filename: filename });
  };
  FormData.prototype.delete = function (name) {
    name = String(name);
    var next = [];
    for (var i = 0; i < this._entries.length; i++) {
      if (this._entries[i].name !== name) next.push(this._entries[i]);
    }
    this._entries = next;
  };
  FormData.prototype.get = function (name) {
    name = String(name);
    for (var i = 0; i < this._entries.length; i++) {
      if (this._entries[i].name === name) return formValueToString(this._entries[i].value);
    }
    return null;
  };
  FormData.prototype.getAll = function (name) {
    name = String(name);
    var o = [];
    for (var i = 0; i < this._entries.length; i++) {
      if (this._entries[i].name === name) o.push(formValueToString(this._entries[i].value));
    }
    return o;
  };
  FormData.prototype.has = function (name) {
    name = String(name);
    for (var i = 0; i < this._entries.length; i++) {
      if (this._entries[i].name === name) return true;
    }
    return false;
  };
  FormData.prototype.set = function (name, value, filename) {
    this.delete(name);
    this.append(name, value, filename);
  };

  function ReadableStream(underlyingSource, strategy) {
    if (!(this instanceof ReadableStream)) throw new TypeError("ReadableStream constructor must be called with new");
    underlyingSource = underlyingSource || {};
    var queue = [];
    var state = "readable";
    var storedError;
    var pending = [];

    function rejectAll(e) {
      while (pending.length) {
        var p = pending.shift();
        p.reject(e);
      }
    }

    function resolveNext() {
      while (pending.length) {
        if (state === "errored") {
          rejectAll(storedError);
          return;
        }
        if (queue.length) {
          var pr = pending.shift();
          pr.resolve({ value: queue.shift(), done: false });
          continue;
        }
        if (state === "closed") {
          var pr2 = pending.shift();
          pr2.resolve({ value: undefined, done: true });
          continue;
        }
        break;
      }
    }

    var controller = {
      enqueue: function (chunk) {
        if (state !== "readable") throw new TypeError("enqueue on non-readable stream");
        queue.push(chunk);
        resolveNext();
      },
      close: function () {
        if (state !== "readable") return;
        state = "closed";
        resolveNext();
      },
      error: function (e) {
        if (state !== "readable") return;
        state = "errored";
        storedError = e;
        rejectAll(e);
      },
    };

    if (typeof underlyingSource.start === "function") {
      underlyingSource.start(controller);
    }

    this._pullRead = function () {
      return new Promise(function (resolve, reject) {
        if (state === "errored") return reject(storedError);
        if (queue.length) return resolve({ value: queue.shift(), done: false });
        if (state === "closed") return resolve({ value: undefined, done: true });
        pending.push({ resolve: resolve, reject: reject });
      });
    };
  }
  ReadableStream.prototype.getReader = function () {
    var stream = this;
    return {
      read: function () {
        return stream._pullRead();
      },
      cancel: function () {
        return Promise.resolve();
      },
      releaseLock: function () {},
    };
  };

  function WritableStream(underlyingSink, strategy) {
    if (!(this instanceof WritableStream)) throw new TypeError("WritableStream constructor must be called with new");
    underlyingSink = underlyingSink || {};
    this._write = underlyingSink.write
      ? function (c) {
          return Promise.resolve(underlyingSink.write(c));
        }
      : function () {
          return Promise.resolve();
        };
    this._close = underlyingSink.close
      ? function () {
          return Promise.resolve(underlyingSink.close());
        }
      : function () {
          return Promise.resolve();
        };
    this._abort = underlyingSink.abort
      ? function (r) {
          return Promise.resolve(underlyingSink.abort(r));
        }
      : function () {
          return Promise.resolve();
        };
  }
  WritableStream.prototype.getWriter = function () {
    var s = this;
    var released = false;
    return {
      write: function (chunk) {
        if (released) return Promise.reject(new TypeError("Writer released"));
        return s._write(chunk);
      },
      close: function () {
        if (released) return Promise.reject(new TypeError("Writer released"));
        return s._close();
      },
      abort: function (reason) {
        return s._abort(reason);
      },
      releaseLock: function () {
        released = true;
      },
    };
  };

  function TransformStream(transformer, writableStrategy, readableStrategy) {
    if (!(this instanceof TransformStream)) throw new TypeError("TransformStream constructor must be called with new");
    transformer = transformer || {};
    var transform = transformer.transform;
    var flush = transformer.flush;
    var readableController;
    var pendingWrite = Promise.resolve();

    var readable = new ReadableStream({
      start: function (c) {
        readableController = c;
      },
    });

    var writable = new WritableStream({
      write: function (chunk) {
        pendingWrite = pendingWrite.then(function () {
          return new Promise(function (res, rej) {
            try {
              if (transform) {
                transform(chunk, {
                  enqueue: function (v) {
                    readableController.enqueue(v);
                  },
                  error: function (e) {
                    readableController.error(e);
                    rej(e);
                  },
                });
              } else {
                readableController.enqueue(chunk);
              }
              res();
            } catch (e) {
              readableController.error(e);
              rej(e);
            }
          });
        });
        return pendingWrite;
      },
      close: function () {
        return pendingWrite.then(function () {
          return new Promise(function (res, rej) {
            try {
              if (flush) {
                flush({
                  enqueue: function (v) {
                    readableController.enqueue(v);
                  },
                });
              }
              readableController.close();
              res();
            } catch (e) {
              rej(e);
            }
          });
        });
      },
      abort: function (reason) {
        try {
          readableController.error(reason);
        } catch (e) {}
        return Promise.resolve();
      },
    });

    this.readable = readable;
    this.writable = writable;
  }

  function ByteLengthQueuingStrategy(init) {
    init = init || {};
    this.highWaterMark = init.highWaterMark != null ? init.highWaterMark : 0;
    this.size = function (chunk) {
      if (chunk && typeof chunk.byteLength === "number") return chunk.byteLength;
      if (typeof chunk === "string") return utf8Encode(chunk).length;
      return 1;
    };
  }

  function CountQueuingStrategy(init) {
    init = init || {};
    this.highWaterMark = init.highWaterMark != null ? init.highWaterMark : 0;
    this.size = function () {
      return 1;
    };
  }

  function ReadableStreamDefaultController() {
    throw new TypeError("Illegal constructor");
  }
  function ReadableStreamDefaultReader() {
    throw new TypeError("Illegal constructor");
  }
  function ReadableByteStreamController() {
    throw new TypeError("Illegal constructor");
  }
  function ReadableStreamBYOBReader() {
    throw new TypeError("Illegal constructor");
  }
  function ReadableStreamBYOBRequest() {
    throw new TypeError("Illegal constructor");
  }
  function WritableStreamDefaultController() {
    throw new TypeError("Illegal constructor");
  }
  function WritableStreamDefaultWriter() {
    throw new TypeError("Illegal constructor");
  }
  function TransformStreamDefaultController() {
    throw new TypeError("Illegal constructor");
  }

  function CompressionStream(format) {
    if (!(this instanceof CompressionStream)) throw new TypeError("Illegal constructor");
    format = String(format || "gzip").toLowerCase();
    if (format !== "gzip" && format !== "deflate") format = "gzip";
    var fmt = format;
    var chunks = [];
    var ts = new TransformStream({
      transform: function (c) {
        var u;
        if (c instanceof ArrayBuffer) u = new Uint8Array(c);
        else if (c instanceof Uint8Array) u = c;
        else u = new Uint8Array(c.buffer, c.byteOffset || 0, c.byteLength);
        chunks.push(new Uint8Array(u));
      },
      flush: function (ctrl) {
        var total = 0;
        for (var i = 0; i < chunks.length; i++) total += chunks[i].length;
        var merged = new Uint8Array(total);
        var off = 0;
        for (var j = 0; j < chunks.length; j++) {
          merged.set(chunks[j], off);
          off += chunks[j].length;
        }
        var ab = __kawkabWebCompress(fmt, merged);
        ctrl.enqueue(new Uint8Array(ab));
      },
    });
    this.readable = ts.readable;
    this.writable = ts.writable;
  }

  function DecompressionStream(format) {
    if (!(this instanceof DecompressionStream)) throw new TypeError("Illegal constructor");
    format = String(format || "gzip").toLowerCase();
    if (format !== "gzip" && format !== "deflate") format = "gzip";
    var fmt = format;
    var chunks = [];
    var ts = new TransformStream({
      transform: function (c) {
        var u;
        if (c instanceof ArrayBuffer) u = new Uint8Array(c);
        else if (c instanceof Uint8Array) u = c;
        else u = new Uint8Array(c.buffer, c.byteOffset || 0, c.byteLength);
        chunks.push(new Uint8Array(u));
      },
      flush: function (ctrl) {
        var total = 0;
        for (var i = 0; i < chunks.length; i++) total += chunks[i].length;
        var merged = new Uint8Array(total);
        var off = 0;
        for (var j = 0; j < chunks.length; j++) {
          merged.set(chunks[j], off);
          off += chunks[j].length;
        }
        var ab = __kawkabWebDecompress(fmt, merged);
        ctrl.enqueue(new Uint8Array(ab));
      },
    });
    this.readable = ts.readable;
    this.writable = ts.writable;
  }

  function MessagePort() {
    EventTarget.call(this);
    this._other = null;
  }
  MessagePort.prototype = Object.create(EventTarget.prototype);
  MessagePort.prototype.constructor = MessagePort;
  MessagePort.prototype.postMessage = function (data) {
    var other = this._other;
    if (!other) return;
    other.dispatchEvent(new MessageEvent("message", { data: data }));
  };
  MessagePort.prototype.start = function () {};
  MessagePort.prototype.close = function () {
    this._other = null;
  };

  function MessageChannel() {
    if (!(this instanceof MessageChannel)) throw new TypeError("Illegal constructor");
    var a = new MessagePort();
    var b = new MessagePort();
    a._other = b;
    b._other = a;
    this.port1 = a;
    this.port2 = b;
  }

  var __bcMap = {};
  function BroadcastChannel(name) {
    if (!(this instanceof BroadcastChannel)) throw new TypeError("Illegal constructor");
    EventTarget.call(this);
    this.name = String(name);
    if (!__bcMap[this.name]) __bcMap[this.name] = [];
    __bcMap[this.name].push(this);
  }
  BroadcastChannel.prototype = Object.create(EventTarget.prototype);
  BroadcastChannel.prototype.constructor = BroadcastChannel;
  BroadcastChannel.prototype.postMessage = function (data) {
    var chs = __bcMap[this.name] || [];
    var self = this;
    for (var i = 0; i < chs.length; i++) {
      if (chs[i] === self) continue;
      chs[i].dispatchEvent(new MessageEvent("message", { data: data }));
    }
  };
  BroadcastChannel.prototype.close = function () {
    var arr = __bcMap[this.name];
    if (!arr) return;
    var j = arr.indexOf(this);
    if (j >= 0) arr.splice(j, 1);
  };

  // WHATWG text streams: do not rely on engine `TextEncoder` / `TextDecoder` (often absent in
  // this QuickJS build). Use the same UTF-8 helpers as Blob / FormData paths.
  var TextDecoderStream = function (encoding, options) {
    if (!(this instanceof TextDecoderStream)) throw new TypeError("Illegal constructor");
    var encArg = encoding != null ? String(encoding) : "utf-8";
    if (encArg !== "utf-8" && encArg.toLowerCase() !== "utf8") {
      throw new TypeError("TextDecoderStream: only utf-8 is supported in this embedding");
    }
    var chunks = [];
    var ts = new TransformStream({
      transform: function (chunk, ctrl) {
        var u;
        if (chunk instanceof ArrayBuffer) u = new Uint8Array(chunk);
        else if (chunk instanceof Uint8Array) u = chunk;
        else if (chunk && typeof chunk.byteLength === "number")
          u = new Uint8Array(chunk.buffer, chunk.byteOffset || 0, chunk.byteLength);
        else u = new Uint8Array(0);
        if (u.byteLength) chunks.push(new Uint8Array(u));
      },
      flush: function (ctrl) {
        var total = 0;
        for (var i = 0; i < chunks.length; i++) total += chunks[i].length;
        var merged = new Uint8Array(total);
        var off = 0;
        for (var j = 0; j < chunks.length; j++) {
          merged.set(chunks[j], off);
          off += chunks[j].length;
        }
        var str = utf8Decode(merged);
        if (str) ctrl.enqueue(str);
      },
    });
    this.readable = ts.readable;
    this.writable = ts.writable;
    this.encoding = "utf-8";
    this.fatal = !!(options && options.fatal);
    this.ignoreBOM = !!(options && options.ignoreBOM);
  };

  var TextEncoderStream = function () {
    if (!(this instanceof TextEncoderStream)) throw new TypeError("Illegal constructor");
    var ts = new TransformStream({
      transform: function (chunk, ctrl) {
        ctrl.enqueue(utf8Encode(String(chunk)));
      },
    });
    this.readable = ts.readable;
    this.writable = ts.writable;
    this.encoding = "utf-8";
  };

  g.TextDecoderStream = TextDecoderStream;
  g.TextEncoderStream = TextEncoderStream;

  // Always install this Atomics object: the engine's native Atomics can abort
  // (SIGABRT) on ordinary TypedArrays in this QuickJS embedding.
  if (!g.__kawkabAtomicsShim) {
    function atomicsBounds(ta, index) {
      if (!ta || typeof ta.length !== "number") throw new TypeError("invalid typed array");
      var i = index >>> 0;
      if (i >= ta.length) throw new RangeError("out of bounds");
      return i;
    }
    g.__kawkabAtomicsShim = true;
    g.Atomics = {
      isLockFree: function (size) {
        var s = size | 0;
        return s === 1 || s === 2 || s === 4 || s === 8;
      },
      load: function (ta, index) {
        var i = atomicsBounds(ta, index);
        return ta[i];
      },
      store: function (ta, index, value) {
        var i = atomicsBounds(ta, index);
        ta[i] = value;
        return value;
      },
      add: function (ta, index, value) {
        var i = atomicsBounds(ta, index);
        var cur = ta[i];
        ta[i] = cur + value;
        return cur;
      },
      sub: function (ta, index, value) {
        var i = atomicsBounds(ta, index);
        var cur = ta[i];
        ta[i] = cur - value;
        return cur;
      },
      and: function (ta, index, value) {
        var i = atomicsBounds(ta, index);
        var cur = ta[i];
        ta[i] = cur & value;
        return cur;
      },
      or: function (ta, index, value) {
        var i = atomicsBounds(ta, index);
        var cur = ta[i];
        ta[i] = cur | value;
        return cur;
      },
      xor: function (ta, index, value) {
        var i = atomicsBounds(ta, index);
        var cur = ta[i];
        ta[i] = cur ^ value;
        return cur;
      },
      exchange: function (ta, index, value) {
        var i = atomicsBounds(ta, index);
        var cur = ta[i];
        ta[i] = value;
        return cur;
      },
      compareExchange: function (ta, index, expected, replacement) {
        var i = atomicsBounds(ta, index);
        var cur = ta[i];
        if (cur === expected) ta[i] = replacement;
        return cur;
      },
      wait: function () {
        throw new TypeError("Atomics.wait is not supported in this embedding");
      },
      notify: function () {
        return 0;
      },
    };
  }

  if (!g.PerformanceObserver) {
    var PerformanceObserver = function (cb) {
      if (!(this instanceof PerformanceObserver)) return new PerformanceObserver(cb);
      this._cb = typeof cb === "function" ? cb : null;
    };
    PerformanceObserver.prototype.observe = function () {};
    PerformanceObserver.prototype.disconnect = function () {};
    PerformanceObserver.prototype.takeRecords = function () {
      return [];
    };
    g.PerformanceObserver = PerformanceObserver;
  }
  if (!g.PerformanceResourceTiming) {
    var PerformanceResourceTiming = function () {
      this.entryType = "resource";
      this.name = "";
    };
    g.PerformanceResourceTiming = PerformanceResourceTiming;
  }
  if (!g.PerformanceObserverEntryList) {
    g.PerformanceObserverEntryList = function () {};
  }

  if (typeof g.__kawkabCryptoRandomBytesSync === "function") {
    function kawkabBufSourceToU8(d) {
      if (d instanceof ArrayBuffer) return new Uint8Array(d);
      if (d && typeof d.byteLength === "number" && d.buffer)
        return new Uint8Array(d.buffer, d.byteOffset || 0, d.byteLength);
      throw new TypeError("invalid BufferSource");
    }
    function kawkabMapDigestAlg(algo) {
      var name = algo && algo.name != null ? String(algo.name) : String(algo || "");
      var u = name.toUpperCase().replace(/-/g, "");
      if (u === "SHA1") return "sha1";
      if (u === "SHA256") return "sha256";
      if (u === "SHA384") return "sha384";
      if (u === "SHA512") return "sha512";
      return null;
    }
    var subtle = {
      digest: function (algo, data) {
        var g0 = g;
        return new Promise(function (resolve, reject) {
          try {
            var a = kawkabMapDigestAlg(algo);
            if (!a) throw new Error("unsupported digest algorithm");
            var u8 = kawkabBufSourceToU8(data);
            var id = g0.__kawkabCryptoCreateHash(a);
            g0.__kawkabCryptoUpdate(id, u8);
            resolve(g0.__kawkabCryptoDigest(id));
          } catch (e) {
            reject(e);
          }
        });
      },
    };
    g.crypto = {
      getRandomValues: function (arr) {
        if (!arr || typeof arr.length !== "number") throw new TypeError("unexpected type");
        var len = arr.length | 0;
        if (len > 65536) throw new TypeError("length out of range");
        if (len < 1) return arr;
        var ab = g.__kawkabCryptoRandomBytesSync(len);
        var src = new Uint8Array(ab);
        for (var i = 0; i < len; i++) arr[i] = src[i];
        return arr;
      },
      randomUUID: function () {
        var ab = g.__kawkabCryptoRandomBytesSync(16);
        var b = new Uint8Array(ab);
        b[6] = (b[6] & 0x0f) | 0x40;
        b[8] = (b[8] & 0x3f) | 0x80;
        var hex = [];
        for (var i = 0; i < 16; i++) hex.push((b[i] < 16 ? "0" : "") + b[i].toString(16));
        return (
          hex.slice(0, 4).join("") +
          "-" +
          hex.slice(4, 6).join("") +
          "-" +
          hex.slice(6, 8).join("") +
          "-" +
          hex.slice(8, 10).join("") +
          "-" +
          hex.slice(10, 16).join("")
        );
      },
      subtle: subtle,
    };
    function CryptoKey() {
      throw new TypeError("Illegal constructor");
    }
    g.CryptoKey = CryptoKey;
    function SubtleCrypto() {
      throw new TypeError("Illegal constructor");
    }
    g.SubtleCrypto = SubtleCrypto;
  }

  g.DOMException = DOMException;
  g.Event = Event;
  g.EventTarget = EventTarget;
  g.CustomEvent = CustomEvent;
  g.MessageEvent = MessageEvent;
  g.Blob = Blob;
  g.FormData = FormData;
  g.ReadableStream = ReadableStream;
  g.WritableStream = WritableStream;
  g.TransformStream = TransformStream;
  g.ByteLengthQueuingStrategy = ByteLengthQueuingStrategy;
  g.CountQueuingStrategy = CountQueuingStrategy;
  g.ReadableStreamDefaultController = ReadableStreamDefaultController;
  g.ReadableStreamDefaultReader = ReadableStreamDefaultReader;
  g.ReadableByteStreamController = ReadableByteStreamController;
  g.ReadableStreamBYOBReader = ReadableStreamBYOBReader;
  g.ReadableStreamBYOBRequest = ReadableStreamBYOBRequest;
  g.WritableStreamDefaultController = WritableStreamDefaultController;
  g.WritableStreamDefaultWriter = WritableStreamDefaultWriter;
  g.TransformStreamDefaultController = TransformStreamDefaultController;
  g.CompressionStream = CompressionStream;
  g.DecompressionStream = DecompressionStream;
  g.MessageChannel = MessageChannel;
  g.MessagePort = MessagePort;
  g.BroadcastChannel = BroadcastChannel;
  if (typeof g.TextEncoder !== "function") {
    g.TextEncoder = function TextEncoder() {
      if (!(this instanceof TextEncoder)) return new TextEncoder();
      this.encoding = "utf-8";
    };
    g.TextEncoder.prototype.encode = function (input) {
      return utf8EncodeManual(input == null ? "" : String(input));
    };
  }
  if (typeof g.TextDecoder !== "function") {
    g.TextDecoder = function TextDecoder(label) {
      if (!(this instanceof TextDecoder)) return new TextDecoder(label);
      var enc = label == null ? "utf-8" : String(label).toLowerCase();
      if (enc !== "utf-8" && enc !== "utf8") {
        throw new TypeError("TextDecoder: only utf-8 is supported in this embedding");
      }
      this.encoding = "utf-8";
    };
    g.TextDecoder.prototype.decode = function (input) {
      if (input == null) return "";
      var u;
      if (input instanceof Uint8Array) u = input;
      else if (input instanceof ArrayBuffer) u = new Uint8Array(input);
      else if (input && typeof input.byteLength === "number")
        u = new Uint8Array(input.buffer, input.byteOffset || 0, input.byteLength);
      else u = new Uint8Array(0);
      return utf8DecodeManual(u);
    };
  }
  if (typeof g.URLSearchParams !== "function") {
    g.URLSearchParams = function URLSearchParams(init) {
      if (!(this instanceof URLSearchParams)) return new URLSearchParams(init);
      this._pairs = [];
      var s = String(init || "");
      if (s.charAt(0) === "?") s = s.slice(1);
      if (!s) return;
      var parts = s.split("&");
      for (var i = 0; i < parts.length; i++) {
        var p = parts[i];
        if (!p) continue;
        var j = p.indexOf("=");
        var k = j >= 0 ? p.slice(0, j) : p;
        var v = j >= 0 ? p.slice(j + 1) : "";
        this._pairs.push([decodeURIComponent(k), decodeURIComponent(v)]);
      }
    };
    g.URLSearchParams.prototype.append = function (k, v) {
      this._pairs.push([String(k), String(v)]);
    };
    g.URLSearchParams.prototype.set = function (k, v) {
      this.delete(k);
      this.append(k, v);
    };
    g.URLSearchParams.prototype.get = function (k) {
      k = String(k);
      for (var i = 0; i < this._pairs.length; i++) if (this._pairs[i][0] === k) return this._pairs[i][1];
      return null;
    };
    g.URLSearchParams.prototype.getAll = function (k) {
      k = String(k);
      var out = [];
      for (var i = 0; i < this._pairs.length; i++) if (this._pairs[i][0] === k) out.push(this._pairs[i][1]);
      return out;
    };
    g.URLSearchParams.prototype.delete = function (k) {
      k = String(k);
      var out = [];
      for (var i = 0; i < this._pairs.length; i++) if (this._pairs[i][0] !== k) out.push(this._pairs[i]);
      this._pairs = out;
    };
    g.URLSearchParams.prototype.toString = function () {
      var out = [];
      for (var i = 0; i < this._pairs.length; i++) out.push(encodeURIComponent(this._pairs[i][0]) + "=" + encodeURIComponent(this._pairs[i][1]));
      return out.join("&");
    };
  }
  if (typeof g.URL !== "function") {
    g.URL = function URL(input, base) {
      if (!(this instanceof URL)) return new URL(input, base);
      var raw = String(input || "");
      var abs = raw.indexOf("://") >= 0 ? raw : (String(base || "http://localhost").replace(/\/$/, "") + "/" + raw.replace(/^\//, ""));
      var qIdx = abs.indexOf("?");
      var hIdx = abs.indexOf("#");
      var endPath = qIdx >= 0 ? qIdx : hIdx >= 0 ? hIdx : abs.length;
      var protoIdx = abs.indexOf("://");
      this.protocol = protoIdx >= 0 ? abs.slice(0, protoIdx + 1) : "http:";
      var hostStart = protoIdx >= 0 ? protoIdx + 3 : 0;
      var slashIdx = abs.indexOf("/", hostStart);
      if (slashIdx < 0) slashIdx = endPath;
      this.host = abs.slice(hostStart, slashIdx);
      this.pathname = abs.slice(slashIdx, endPath) || "/";
      this.search = qIdx >= 0 ? abs.slice(qIdx, hIdx >= 0 ? hIdx : abs.length) : "";
      this.hash = hIdx >= 0 ? abs.slice(hIdx) : "";
      this.searchParams = new g.URLSearchParams(this.search);
    };
    Object.defineProperty(g.URL.prototype, "href", {
      get: function () {
        var s = this.searchParams && this.searchParams.toString ? this.searchParams.toString() : "";
        var q = s ? "?" + s : "";
        return this.protocol + "//" + this.host + this.pathname + q + this.hash;
      },
    });
    g.URL.prototype.toString = function () {
      return this.href;
    };
  }
  if (typeof g.Headers !== "function") {
    g.Headers = function Headers(init) {
      if (!(this instanceof Headers)) return new Headers(init);
      this._map = {};
      if (!init) return;
      if (Array.isArray(init)) {
        for (var i = 0; i < init.length; i++) this.append(init[i][0], init[i][1]);
      } else if (typeof init === "object") {
        var ks = Object.keys(init);
        for (var j = 0; j < ks.length; j++) this.append(ks[j], init[ks[j]]);
      }
    };
    g.Headers.prototype.append = function (k, v) {
      k = String(k).toLowerCase();
      v = String(v);
      this._map[k] = this._map[k] ? this._map[k] + ", " + v : v;
    };
    g.Headers.prototype.set = function (k, v) {
      this._map[String(k).toLowerCase()] = String(v);
    };
    g.Headers.prototype.get = function (k) {
      k = String(k).toLowerCase();
      return Object.prototype.hasOwnProperty.call(this._map, k) ? this._map[k] : null;
    };
    g.Headers.prototype.has = function (k) {
      return Object.prototype.hasOwnProperty.call(this._map, String(k).toLowerCase());
    };
    g.Headers.prototype.delete = function (k) {
      delete this._map[String(k).toLowerCase()];
    };
  }
  if (typeof g.Request !== "function") {
    g.Request = function Request(input, init) {
      if (!(this instanceof Request)) return new Request(input, init);
      init = init || {};
      this.url = String(input || "");
      this.method = String(init.method || "GET").toUpperCase();
      this.headers = init.headers instanceof g.Headers ? init.headers : new g.Headers(init.headers);
      this.body = init.body == null ? null : init.body;
    };
  }
  if (typeof g.Response !== "function") {
    g.Response = function Response(body, init) {
      if (!(this instanceof Response)) return new Response(body, init);
      init = init || {};
      this.status = Number(init.status || 200);
      this.statusText = String(init.statusText || "");
      this.headers = init.headers instanceof g.Headers ? init.headers : new g.Headers(init.headers);
      this._body = body == null ? "" : body;
      this.ok = this.status >= 200 && this.status < 300;
    };
    g.Response.prototype.text = function () {
      if (typeof this._body === "string") return Promise.resolve(this._body);
      if (this._body instanceof Uint8Array) return Promise.resolve(utf8Decode(this._body));
      if (this._body instanceof ArrayBuffer) return Promise.resolve(utf8Decode(new Uint8Array(this._body)));
      return Promise.resolve(String(this._body));
    };
    g.Response.prototype.arrayBuffer = function () {
      if (this._body instanceof ArrayBuffer) return Promise.resolve(this._body);
      if (this._body instanceof Uint8Array) return Promise.resolve(this._body.buffer.slice(0));
      return Promise.resolve(utf8Encode(String(this._body)).buffer);
    };
  }
  if (typeof g.fetch !== "function") {
    g.fetch = function fetch(input, init) {
      var req = input instanceof g.Request ? input : new g.Request(input, init);
      return Promise.resolve(new g.Response("", { status: 200, headers: req.headers }));
    };
  }
  if (typeof g.WebAssembly !== "object" || !g.WebAssembly) {
    function WasmCompileError(msg) {
      this.name = "CompileError";
      this.message = String(msg || "WebAssembly compile error");
    }
    WasmCompileError.prototype = Object.create(Error.prototype);
    WasmCompileError.prototype.constructor = WasmCompileError;
    function WasmLinkError(msg) {
      this.name = "LinkError";
      this.message = String(msg || "WebAssembly link error");
    }
    WasmLinkError.prototype = Object.create(Error.prototype);
    WasmLinkError.prototype.constructor = WasmLinkError;
    function WasmRuntimeError(msg) {
      this.name = "RuntimeError";
      this.message = String(msg || "WebAssembly runtime error");
    }
    WasmRuntimeError.prototype = Object.create(Error.prototype);
    WasmRuntimeError.prototype.constructor = WasmRuntimeError;

    function toWasmU8(bytes) {
      if (bytes instanceof Uint8Array) return bytes;
      if (bytes instanceof ArrayBuffer) return new Uint8Array(bytes);
      if (bytes && typeof bytes.byteLength === "number" && bytes.buffer)
        return new Uint8Array(bytes.buffer, bytes.byteOffset || 0, bytes.byteLength);
      throw new TypeError("WebAssembly bytes must be a BufferSource");
    }
    function wasmLooksValid(bytes) {
      var u8 = toWasmU8(bytes);
      return (
        u8.length >= 8 &&
        u8[0] === 0x00 &&
        u8[1] === 0x61 &&
        u8[2] === 0x73 &&
        u8[3] === 0x6d &&
        u8[4] === 0x01 &&
        u8[5] === 0x00 &&
        u8[6] === 0x00 &&
        u8[7] === 0x00
      );
    }
    function Module(bytes) {
      if (!(this instanceof Module)) throw new TypeError("Illegal constructor");
      if (!wasmLooksValid(bytes)) throw new WasmCompileError("invalid wasm module bytes");
      this._bytes = toWasmU8(bytes);
    }
    function Instance(module, imports) {
      if (!(this instanceof Instance)) throw new TypeError("Illegal constructor");
      if (!(module instanceof Module)) throw new TypeError("first argument must be a WebAssembly.Module");
      this.module = module;
      this.imports = imports || {};
      this.exports = {};
    }
    function Memory(desc) {
      if (!(this instanceof Memory)) throw new TypeError("Illegal constructor");
      desc = desc || {};
      var pages = Number(desc.initial || 0);
      this.buffer = new ArrayBuffer(Math.max(0, pages | 0) * 65536);
    }
    function Table(desc) {
      if (!(this instanceof Table)) throw new TypeError("Illegal constructor");
      desc = desc || {};
      this.length = Number(desc.initial || 0) | 0;
    }
    function Global(desc, value) {
      if (!(this instanceof Global)) throw new TypeError("Illegal constructor");
      this.value = value;
      this.mutable = !!(desc && desc.mutable);
    }
    g.WebAssembly = {
      Module: Module,
      Instance: Instance,
      Memory: Memory,
      Table: Table,
      Global: Global,
      CompileError: WasmCompileError,
      LinkError: WasmLinkError,
      RuntimeError: WasmRuntimeError,
      validate: function (bytes) {
        try {
          return wasmLooksValid(bytes);
        } catch (_e) {
          return false;
        }
      },
      compile: function (bytes) {
        return new Promise(function (resolve, reject) {
          try {
            resolve(new Module(bytes));
          } catch (e) {
            reject(e);
          }
        });
      },
      instantiate: function (source, imports) {
        return new Promise(function (resolve, reject) {
          try {
            if (source instanceof Module) {
              var i = new Instance(source, imports);
              resolve({ module: source, instance: i });
              return;
            }
            var m = new Module(source);
            var inst = new Instance(m, imports);
            resolve({ module: m, instance: inst });
          } catch (e) {
            reject(e);
          }
        });
      },
    };
  }

  return true;
})();
