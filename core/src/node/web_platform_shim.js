(function () {
  var g = typeof globalThis !== "undefined" ? globalThis : typeof global !== "undefined" ? global : this;

  function utf8Encode(str) {
    str = String(str);
    if (typeof TextEncoder !== "undefined") return new TextEncoder().encode(str);
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

  function utf8Decode(bytes) {
    if (typeof TextDecoder !== "undefined") return new TextDecoder("utf-8").decode(bytes);
    var s = "";
    for (var i = 0; i < bytes.length; i++) s += String.fromCharCode(bytes[i]);
    try {
      return decodeURIComponent(escape(s));
    } catch (e) {
      return s;
    }
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

  return true;
})();
