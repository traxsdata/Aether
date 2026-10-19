// prelude.js - the parts of the web platform æther's browser writes in
// script itself, on top of a few natives (the "@@" globals, which pages
// can't name as identifiers). It runs once, before a page's own scripts.
//
// Requests stay on the page's own site (the @@fetch native enforces it),
// random numbers come from the kernel, and nothing is stored anywhere.
(function () {
  'use strict';
  var G = globalThis;
  var def = G['@@def'];
  var parseURL = G['@@parseURL'];
  var rawFetch = G['@@fetch'];
  var xhrSend = G['@@xhrSend'];
  var rnd = G['@@random'];

  function hidden(o, k, v) {
    Object.defineProperty(o, k, { value: v, writable: true, configurable: true, enumerable: false });
  }

  // NodeList and HTMLCollection are plain arrays here.
  hidden(Array.prototype, 'item', function (i) {
    var v = this[i >>> 0];
    return v === undefined ? null : v;
  });
  ['NodeList', 'HTMLCollection', 'HTMLAllCollection', 'HTMLOptionsCollection', 'DOMTokenList', 'CSSStyleDeclaration', 'NamedNodeMap', 'DOMStringMap'].forEach(function (n) {
    var C = function () { throw new TypeError('Illegal constructor'); };
    C.prototype = Array.prototype;
    def(n, C);
  });

  class DOMException extends Error {
    constructor(message, name) {
      super(message === undefined ? '' : String(message));
      this.name = name === undefined ? 'Error' : String(name);
      this.code = 0;
    }
  }
  def('DOMException', DOMException);

  // ---- URLs ----------------------------------------------------------------
  function decodeForm(s) {
    try {
      return decodeURIComponent(s.replace(/\+/g, ' '));
    } catch (e) {
      return s;
    }
  }
  function encodeForm(s) {
    return encodeURIComponent(s).replace(/%20/g, '+').replace(/[!'()~]/g, function (c) {
      return '%' + c.charCodeAt(0).toString(16).toUpperCase();
    });
  }

  class URLSearchParams {
    constructor(init) {
      hidden(this, '_list', []);
      hidden(this, '_url', null);
      if (init === undefined || init === null) return;
      if (init instanceof URLSearchParams) {
        init.forEach((v, k) => this._list.push([k, v]));
      } else if (typeof init === 'object') {
        if (typeof init[Symbol.iterator] === 'function') {
          for (const pair of init) this._list.push([String(pair[0]), String(pair[1])]);
        } else {
          for (const k of Object.keys(init)) this._list.push([k, String(init[k])]);
        }
      } else {
        this._parse(String(init));
      }
    }
    _parse(s) {
      this._list.length = 0;
      if (s.charAt(0) === '?') s = s.slice(1);
      if (!s) return;
      for (const part of s.split('&')) {
        if (!part) continue;
        const i = part.indexOf('=');
        const k = i < 0 ? part : part.slice(0, i);
        const v = i < 0 ? '' : part.slice(i + 1);
        this._list.push([decodeForm(k), decodeForm(v)]);
      }
    }
    _changed() {
      if (this._url) this._url._setSearch(this.toString());
    }
    append(k, v) {
      this._list.push([String(k), String(v)]);
      this._changed();
    }
    delete(k) {
      k = String(k);
      this._list = this._list.filter((p) => p[0] !== k);
      this._changed();
    }
    get(k) {
      k = String(k);
      const p = this._list.find((p) => p[0] === k);
      return p ? p[1] : null;
    }
    getAll(k) {
      k = String(k);
      return this._list.filter((p) => p[0] === k).map((p) => p[1]);
    }
    has(k) {
      k = String(k);
      return this._list.some((p) => p[0] === k);
    }
    set(k, v) {
      k = String(k);
      v = String(v);
      const i = this._list.findIndex((p) => p[0] === k);
      if (i < 0) this._list.push([k, v]);
      else {
        this._list[i][1] = v;
        this._list = this._list.filter((p, j) => j <= i || p[0] !== k);
      }
      this._changed();
    }
    sort() {
      this._list.sort((a, b) => (a[0] < b[0] ? -1 : a[0] > b[0] ? 1 : 0));
      this._changed();
    }
    forEach(f, thisArg) {
      for (const p of this._list.slice()) f.call(thisArg, p[1], p[0], this);
    }
    keys() {
      return this._list.map((p) => p[0])[Symbol.iterator]();
    }
    values() {
      return this._list.map((p) => p[1])[Symbol.iterator]();
    }
    entries() {
      return this._list.map((p) => [p[0], p[1]])[Symbol.iterator]();
    }
    [Symbol.iterator]() {
      return this.entries();
    }
    get size() {
      return this._list.length;
    }
    toString() {
      return this._list.map((p) => encodeForm(p[0]) + '=' + encodeForm(p[1])).join('&');
    }
  }
  def('URLSearchParams', URLSearchParams);

  class URL {
    constructor(url, base) {
      this._set(parseURL(String(url), base === undefined ? undefined : String(base)));
    }
    _set(p) {
      hidden(this, '_p', p);
      if (this._sp) this._sp._parse(p.search);
    }
    _setSearch(q) {
      const p = this._p;
      this._set(parseURL(p.origin + p.pathname + (q ? '?' + q : '') + p.hash));
    }
    get href() { return this._p.href; }
    set href(v) { this._set(parseURL(String(v))); }
    get protocol() { return this._p.protocol; }
    set protocol(v) {
      const p = this._p;
      this._set(parseURL(String(v).replace(/:?$/, ':') + '//' + p.host + p.pathname + p.search + p.hash));
    }
    get host() { return this._p.host; }
    set host(v) {
      const p = this._p;
      this._set(parseURL(p.protocol + '//' + String(v) + p.pathname + p.search + p.hash));
    }
    get hostname() { return this._p.hostname; }
    set hostname(v) {
      const p = this._p;
      this._set(parseURL(p.protocol + '//' + String(v) + (p.port ? ':' + p.port : '') + p.pathname + p.search + p.hash));
    }
    get port() { return this._p.port; }
    set port(v) {
      const p = this._p;
      this._set(parseURL(p.protocol + '//' + p.hostname + (String(v) ? ':' + v : '') + p.pathname + p.search + p.hash));
    }
    get pathname() { return this._p.pathname; }
    set pathname(v) {
      const p = this._p;
      v = String(v);
      this._set(parseURL(p.origin + (v.charAt(0) === '/' ? v : '/' + v) + p.search + p.hash));
    }
    get search() { return this._p.search; }
    set search(v) {
      v = String(v);
      this._setSearch(v.charAt(0) === '?' ? v.slice(1) : v);
    }
    get hash() { return this._p.hash; }
    set hash(v) {
      const p = this._p;
      v = String(v);
      this._set(parseURL(p.origin + p.pathname + p.search + (v ? '#' + v.replace(/^#/, '') : '')));
    }
    get origin() { return this._p.origin; }
    get username() { return ''; }
    get password() { return ''; }
    get searchParams() {
      if (!this._sp) {
        hidden(this, '_sp', new URLSearchParams(this._p.search));
        this._sp._url = this;
      }
      return this._sp;
    }
    toString() { return this._p.href; }
    toJSON() { return this._p.href; }
    static canParse(u, b) {
      try {
        new URL(u, b);
        return true;
      } catch (e) {
        return false;
      }
    }
    static createObjectURL() { return 'blob:null/0'; }
    static revokeObjectURL() {}
  }
  def('URL', URL);
  def('webkitURL', URL);

  // ---- requests --------------------------------------------------------------
  class Headers {
    constructor(init) {
      hidden(this, '_h', []);
      if (!init) return;
      if (init instanceof Headers) init.forEach((v, k) => this.append(k, v));
      else if (typeof init[Symbol.iterator] === 'function') for (const p of init) this.append(p[0], p[1]);
      else for (const k of Object.keys(init)) this.append(k, init[k]);
    }
    append(k, v) { this._h.push([String(k).toLowerCase(), String(v)]); }
    set(k, v) {
      k = String(k).toLowerCase();
      this._h = this._h.filter((p) => p[0] !== k);
      this._h.push([k, String(v)]);
    }
    get(k) {
      k = String(k).toLowerCase();
      const v = this._h.filter((p) => p[0] === k).map((p) => p[1]);
      return v.length ? v.join(', ') : null;
    }
    has(k) {
      k = String(k).toLowerCase();
      return this._h.some((p) => p[0] === k);
    }
    delete(k) {
      k = String(k).toLowerCase();
      this._h = this._h.filter((p) => p[0] !== k);
    }
    forEach(f, thisArg) {
      for (const p of this._h.slice()) f.call(thisArg, p[1], p[0], this);
    }
    keys() { return this._h.map((p) => p[0])[Symbol.iterator](); }
    values() { return this._h.map((p) => p[1])[Symbol.iterator](); }
    entries() { return this._h.map((p) => [p[0], p[1]])[Symbol.iterator](); }
    [Symbol.iterator]() { return this.entries(); }
  }
  def('Headers', Headers);

  class Blob {
    constructor(parts, opts) {
      hidden(this, '_text', (parts || []).map((p) => (p instanceof Blob ? p._text : String(p))).join(''));
      this.type = (opts && opts.type) || '';
    }
    get size() { return this._text.length; }
    text() { return Promise.resolve(this._text); }
    slice(a, b, t) { return new Blob([this._text.slice(a, b)], { type: t }); }
    arrayBuffer() { return Promise.reject(new TypeError('binary data is not supported')); }
  }
  def('Blob', Blob);
  class File extends Blob {
    constructor(parts, name, opts) {
      super(parts, opts);
      this.name = String(name);
      this.lastModified = Date.now();
    }
  }
  def('File', File);

  class FormData {
    constructor(form) {
      hidden(this, '_list', []);
      if (!form) return;
      for (const el of form.elements) {
        if (!el.name || el.disabled) continue;
        const t = el.type;
        if ((t === 'checkbox' || t === 'radio') && !el.checked) continue;
        if (t === 'submit' || t === 'button' || t === 'reset' || t === 'file' || t === 'image') continue;
        if (el.tagName === 'FIELDSET' || el.tagName === 'BUTTON' || el.tagName === 'OUTPUT') continue;
        this._list.push([el.name, el.value]);
      }
    }
    append(k, v) { this._list.push([String(k), v instanceof Blob ? v : String(v)]); }
    set(k, v) {
      k = String(k);
      this._list = this._list.filter((p) => p[0] !== k);
      this.append(k, v);
    }
    get(k) {
      const p = this._list.find((p) => p[0] === String(k));
      return p ? p[1] : null;
    }
    getAll(k) { return this._list.filter((p) => p[0] === String(k)).map((p) => p[1]); }
    has(k) { return this._list.some((p) => p[0] === String(k)); }
    delete(k) { this._list = this._list.filter((p) => p[0] !== String(k)); }
    forEach(f, thisArg) { for (const p of this._list.slice()) f.call(thisArg, p[1], p[0], this); }
    keys() { return this._list.map((p) => p[0])[Symbol.iterator](); }
    values() { return this._list.map((p) => p[1])[Symbol.iterator](); }
    entries() { return this._list.map((p) => [p[0], p[1]])[Symbol.iterator](); }
    [Symbol.iterator]() { return this.entries(); }
    _encode() {
      return this._list.map((p) => encodeForm(p[0]) + '=' + encodeForm(p[1] instanceof Blob ? p[1]._text : p[1])).join('&');
    }
  }
  def('FormData', FormData);

  function bodyText(b) {
    if (b === undefined || b === null) return null;
    if (typeof b === 'string') return b;
    if (b instanceof URLSearchParams) return b.toString();
    if (b instanceof FormData) return b._encode();
    if (b instanceof Blob) return b._text;
    return String(b);
  }

  class Response {
    constructor(body, init) {
      init = init || {};
      hidden(this, '_body', body === undefined || body === null ? '' : String(body));
      this.status = init.status === undefined ? 200 : init.status;
      this.statusText = init.statusText || '';
      this.ok = this.status >= 200 && this.status < 300;
      this.headers = init.headers instanceof Headers ? init.headers : new Headers(init.headers);
      this.url = init.url || '';
      this.type = 'basic';
      this.redirected = !!init.redirected;
      this.bodyUsed = false;
      this.body = null;
    }
    text() {
      this.bodyUsed = true;
      return Promise.resolve(this._body);
    }
    json() {
      const b = this._body;
      this.bodyUsed = true;
      return new Promise((res) => res(JSON.parse(b)));
    }
    blob() { return Promise.resolve(new Blob([this._body])); }
    formData() { return Promise.reject(new TypeError('formData() is not supported')); }
    arrayBuffer() { return Promise.reject(new TypeError('binary data is not supported')); }
    clone() {
      return new Response(this._body, { status: this.status, statusText: this.statusText, headers: new Headers(this.headers), url: this.url });
    }
    static json(data, init) {
      const r = new Response(JSON.stringify(data), init);
      r.headers.set('content-type', 'application/json');
      return r;
    }
    static error() { return new Response('', { status: 0 }); }
  }
  def('Response', Response);

  class Request {
    constructor(input, init) {
      init = init || {};
      this.url = input instanceof Request ? input.url : new URL(String(input), location.href).href;
      this.method = String(init.method || (input instanceof Request ? input.method : 'GET')).toUpperCase();
      this.headers = new Headers(init.headers || (input instanceof Request ? input.headers : undefined));
      hidden(this, '_body', init.body !== undefined ? init.body : input instanceof Request ? input._body : undefined);
      this.signal = init.signal || (input instanceof Request ? input.signal : null);
      this.credentials = 'same-origin';
      this.mode = 'same-origin';
    }
    clone() { return new Request(this); }
    text() { return Promise.resolve(bodyText(this._body) || ''); }
  }
  def('Request', Request);

  def('fetch', function fetch(input, init) {
    init = init || {};
    let url;
    if (input instanceof Request) url = input.url;
    else if (input instanceof URL) url = input.href;
    else url = String(input);
    const method = init.method || (input instanceof Request ? input.method : 'GET');
    const body = init.body !== undefined ? init.body : input instanceof Request ? input._body : undefined;
    const signal = init.signal || (input instanceof Request ? input.signal : null);
    if (signal && signal.aborted) {
      return Promise.reject(signal.reason || new DOMException('The operation was aborted.', 'AbortError'));
    }
    return rawFetch(url, method, bodyText(body)).then((p) => new Response(p[3], { status: p[0], url: p[1], headers: p[2], redirected: p[1] !== url }));
  });

  class AbortSignal extends EventTarget {
    constructor() {
      super();
      this.aborted = false;
      this.reason = undefined;
      this.onabort = null;
    }
    throwIfAborted() {
      if (this.aborted) throw this.reason;
    }
    static abort(reason) {
      const s = new AbortSignal();
      s.aborted = true;
      s.reason = reason === undefined ? new DOMException('The operation was aborted.', 'AbortError') : reason;
      return s;
    }
    static timeout(ms) {
      const c = new AbortController();
      setTimeout(() => c.abort(new DOMException('The operation timed out.', 'TimeoutError')), ms);
      return c.signal;
    }
    static any(signals) {
      const c = new AbortController();
      for (const s of signals) {
        if (s.aborted) {
          c.abort(s.reason);
          break;
        }
        s.addEventListener('abort', () => c.abort(s.reason));
      }
      return c.signal;
    }
  }
  def('AbortSignal', AbortSignal);
  class AbortController {
    constructor() {
      this.signal = new AbortSignal();
    }
    abort(reason) {
      const s = this.signal;
      if (s.aborted) return;
      s.aborted = true;
      s.reason = reason === undefined ? new DOMException('The operation was aborted.', 'AbortError') : reason;
      s.dispatchEvent(new Event('abort'));
    }
  }
  def('AbortController', AbortController);

  class XMLHttpRequest extends EventTarget {
    constructor() {
      super();
      this.readyState = 0;
      this.status = 0;
      this.statusText = '';
      this.responseText = '';
      this.response = '';
      this.responseType = '';
      this.responseURL = '';
      this.responseXML = null;
      this.timeout = 0;
      this.withCredentials = false;
      this.upload = new EventTarget();
      for (const h of ['readystatechange', 'load', 'error', 'loadend', 'loadstart', 'abort', 'timeout', 'progress']) this['on' + h] = null;
      hidden(this, '_h', []);
    }
    open(method, url, async) {
      hidden(this, '_m', String(method).toUpperCase());
      hidden(this, '_u', String(url));
      hidden(this, '_async', async !== false);
      hidden(this, '_aborted', false);
      this.readyState = 1;
      this._fire('readystatechange');
    }
    setRequestHeader() {}
    overrideMimeType() {}
    getResponseHeader(n) {
      n = String(n).toLowerCase();
      const v = this._h.filter((p) => p[0] === n).map((p) => p[1]);
      return v.length ? v.join(', ') : null;
    }
    getAllResponseHeaders() {
      return this._h.map((p) => p[0] + ': ' + p[1] + '\r\n').join('');
    }
    abort() {
      this._aborted = true;
      if (this.readyState > 0 && this.readyState < 4) {
        this.readyState = 4;
        this._fire('readystatechange');
        this._fire('abort');
        this._fire('loadend');
      }
      this.readyState = 0;
    }
    send(body) {
      if (this.readyState !== 1) throw new DOMException('send() before open()', 'InvalidStateError');
      this._fire('loadstart');
      xhrSend(this, this._u, this._m, this._m === 'GET' || this._m === 'HEAD' ? null : bodyText(body), !this._async);
    }
    _fire(type) {
      this.dispatchEvent(new Event(type));
    }
    ['@@done'](p) {
      if (this._aborted) return;
      this.status = p[0];
      this.statusText = p[0] === 200 ? 'OK' : '';
      this.responseURL = p[1];
      this._h = p[2];
      this.responseText = p[3];
      if (this.responseType === 'json') {
        try {
          this.response = JSON.parse(p[3]);
        } catch (e) {
          this.response = null;
        }
      } else if (this.responseType === 'document' || this.responseType === 'arraybuffer' || this.responseType === 'blob') {
        this.response = this.responseType === 'blob' ? new Blob([p[3]]) : null;
      } else {
        this.response = p[3];
      }
      this.readyState = 2;
      this._fire('readystatechange');
      this.readyState = 3;
      this._fire('readystatechange');
      this.readyState = 4;
      this._fire('readystatechange');
      this._fire('load');
      this._fire('loadend');
    }
    ['@@fail'](msg) {
      if (this._aborted) return;
      console.warn('XMLHttpRequest: ' + msg);
      this.readyState = 4;
      this.status = 0;
      this._fire('readystatechange');
      this._fire('error');
      this._fire('loadend');
    }
  }
  XMLHttpRequest.UNSENT = 0;
  XMLHttpRequest.OPENED = 1;
  XMLHttpRequest.HEADERS_RECEIVED = 2;
  XMLHttpRequest.LOADING = 3;
  XMLHttpRequest.DONE = 4;
  def('XMLHttpRequest', XMLHttpRequest);

  // ---- messages -------------------------------------------------------------
  class MessagePort extends EventTarget {
    constructor() {
      super();
      this.onmessage = null;
      hidden(this, '_other', null);
    }
    postMessage(data) {
      const o = this._other;
      setTimeout(() => {
        const e = new Event('message');
        e.data = data;
        o.dispatchEvent(e);
      }, 0);
    }
    start() {}
    close() {}
  }
  def('MessagePort', MessagePort);
  def('MessageChannel', class MessageChannel {
    constructor() {
      this.port1 = new MessagePort();
      this.port2 = new MessagePort();
      this.port1._other = this.port2;
      this.port2._other = this.port1;
    }
  });
  def('BroadcastChannel', class BroadcastChannel extends EventTarget {
    constructor(name) {
      super();
      this.name = String(name);
      this.onmessage = null;
    }
    postMessage() {}
    close() {}
  });
  def('@@deliverMessage', function (data) {
    const e = new Event('message');
    e.data = data;
    e.origin = location.origin;
    e.source = window;
    window.dispatchEvent(e);
  });

  // ---- observers ------------------------------------------------------------
  // Everything counts as visible: lazy-loading pages load all their content.
  class IntersectionObserver {
    constructor(cb, opts) {
      hidden(this, '_cb', cb);
      hidden(this, '_t', []);
      this.root = (opts && opts.root) || null;
      this.rootMargin = (opts && opts.rootMargin) || '0px';
      this.thresholds = [0];
    }
    observe(el) {
      this._t.push(el);
      setTimeout(() => {
        if (this._t.indexOf(el) < 0) return;
        const r = el.getBoundingClientRect();
        this._cb([{ target: el, isIntersecting: true, intersectionRatio: 1, boundingClientRect: r, intersectionRect: r, rootBounds: null, time: performance.now() }], this);
      }, 0);
    }
    unobserve(el) {
      const i = this._t.indexOf(el);
      if (i >= 0) this._t.splice(i, 1);
    }
    disconnect() { this._t.length = 0; }
    takeRecords() { return []; }
  }
  def('IntersectionObserver', IntersectionObserver);
  def('ResizeObserver', class ResizeObserver {
    constructor(cb) {
      hidden(this, '_cb', cb);
      hidden(this, '_t', []);
    }
    observe(el) {
      this._t.push(el);
      setTimeout(() => {
        if (this._t.indexOf(el) < 0) return;
        const r = el.getBoundingClientRect();
        const size = [{ inlineSize: r.width, blockSize: r.height }];
        this._cb([{ target: el, contentRect: r, borderBoxSize: size, contentBoxSize: size }], this);
      }, 0);
    }
    unobserve(el) {
      const i = this._t.indexOf(el);
      if (i >= 0) this._t.splice(i, 1);
    }
    disconnect() { this._t.length = 0; }
  });
  def('MutationObserver', class MutationObserver {
    constructor(cb) { hidden(this, '_cb', cb); }
    observe() {}
    disconnect() {}
    takeRecords() { return []; }
  });
  const PerformanceObserver = class PerformanceObserver {
    observe() {}
    disconnect() {}
    takeRecords() { return []; }
  };
  PerformanceObserver.supportedEntryTypes = [];
  def('PerformanceObserver', PerformanceObserver);

  // ---- assorted ---------------------------------------------------------------
  const registry = new Map();
  def('customElements', {
    define(name, cls) { registry.set(String(name), cls); },
    get(name) { return registry.get(String(name)); },
    whenDefined(name) { return Promise.resolve(registry.get(String(name))); },
    upgrade() {},
  });
  def('CSS', {
    supports() { return false; },
    escape(s) { return String(s).replace(/[^a-zA-Z0-9_ -￿-]/g, (c) => '\\' + c); },
  });

  def('Image', function Image(w, h) {
    const i = document.createElement('img');
    if (w !== undefined) i.width = String(w);
    if (h !== undefined) i.height = String(h);
    return i;
  });
  def('Audio', function Audio(src) {
    const a = document.createElement('audio');
    if (src !== undefined) a.src = String(src);
    return a;
  });
  def('Option', function Option(text, value, def, selected) {
    const o = document.createElement('option');
    if (text !== undefined) o.textContent = String(text);
    if (value !== undefined) o.value = String(value);
    if (selected) o.selected = true;
    return o;
  });

  def('DOMParser', class DOMParser {
    parseFromString(text) {
      const root = document.createElement('div');
      root.innerHTML = String(text);
      return {
        body: root,
        documentElement: root,
        head: null,
        title: '',
        querySelector: (s) => root.querySelector(s),
        querySelectorAll: (s) => root.querySelectorAll(s),
        getElementsByTagName: (t) => root.getElementsByTagName(t),
        getElementsByClassName: (c) => root.getElementsByClassName(c),
        getElementById: (id) => root.querySelector('#' + CSS.escape(id)),
      };
    }
  });

  // Base64.
  const B64 = 'ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/';
  def('btoa', function btoa(s) {
    s = String(s);
    let out = '';
    for (let i = 0; i < s.length; i += 3) {
      const a = s.charCodeAt(i), b = s.charCodeAt(i + 1), c = s.charCodeAt(i + 2);
      if (a > 255 || b > 255 || c > 255) throw new DOMException('The string contains characters outside of the Latin1 range.', 'InvalidCharacterError');
      const n = (a << 16) | ((b || 0) << 8) | (c || 0);
      out += B64[(n >> 18) & 63] + B64[(n >> 12) & 63] + (i + 1 < s.length ? B64[(n >> 6) & 63] : '=') + (i + 2 < s.length ? B64[n & 63] : '=');
    }
    return out;
  });
  def('atob', function atob(s) {
    s = String(s).replace(/[\t\n\f\r ]/g, '');
    if (s.length % 4 === 0) s = s.replace(/==?$/, '');
    if (s.length % 4 === 1 || /[^A-Za-z0-9+/]/.test(s)) throw new DOMException('The string to be decoded is not correctly encoded.', 'InvalidCharacterError');
    let out = '', bits = 0, n = 0;
    for (let i = 0; i < s.length; i++) {
      n = (n << 6) | B64.indexOf(s[i]);
      bits += 6;
      if (bits >= 8) {
        bits -= 8;
        out += String.fromCharCode((n >> bits) & 255);
      }
    }
    return out;
  });

  // Typed arrays, as plain arrays of numbers (no shared buffers).
  function ArrayBuffer(n) {
    if (!(this instanceof ArrayBuffer)) throw new TypeError("Constructor ArrayBuffer requires 'new'");
    this.byteLength = n >>> 0;
  }
  ArrayBuffer.isView = (v) => Array.isArray(v) && typeof v.BYTES_PER_ELEMENT === 'number';
  def('ArrayBuffer', ArrayBuffer);
  const conv = {
    Uint8Array: [1, (v) => v & 255],
    Uint8ClampedArray: [1, (v) => Math.max(0, Math.min(255, Math.round(v)))],
    Int8Array: [1, (v) => ((v & 255) << 24) >> 24],
    Uint16Array: [2, (v) => v & 65535],
    Int16Array: [2, (v) => ((v & 65535) << 16) >> 16],
    Uint32Array: [4, (v) => v >>> 0],
    Int32Array: [4, (v) => v | 0],
    Float32Array: [4, (v) => +v],
    Float64Array: [8, (v) => +v],
  };
  Object.keys(conv).forEach((name) => {
    const bytes = conv[name][0], c = conv[name][1];
    function make(arr) {
      for (let i = 0; i < arr.length; i++) arr[i] = c(arr[i]) || 0;
      hidden(arr, 'BYTES_PER_ELEMENT', bytes);
      hidden(arr, 'byteLength', arr.length * bytes);
      hidden(arr, 'byteOffset', 0);
      hidden(arr, 'buffer', new ArrayBuffer(arr.length * bytes));
      hidden(arr, 'set', function (src, off) {
        off = off | 0;
        for (let i = 0; i < src.length; i++) this[off + i] = c(src[i]);
      });
      hidden(arr, 'subarray', function (a, b) { return make(this.slice(a, b)); });
      return arr;
    }
    const T = function (a) {
      if (a === undefined || typeof a === 'number') return make(new Array(a >>> 0).fill(0));
      if (a instanceof ArrayBuffer) return make(new Array(Math.floor(a.byteLength / bytes)).fill(0));
      return make(Array.from(a));
    };
    T.BYTES_PER_ELEMENT = bytes;
    T.from = (x, f) => make(Array.from(x, f));
    T.of = (...x) => make(x);
    def(name, T);
  });

  def('crypto', {
    getRandomValues(arr) {
      if (!Array.isArray(arr)) throw new TypeError('getRandomValues: not an integer array');
      const per = arr.BYTES_PER_ELEMENT || 1;
      if (arr.length * per > 65536) throw new DOMException('The requested length exceeds 65,536 bytes', 'QuotaExceededError');
      const b = rnd(arr.length * per);
      for (let i = 0; i < arr.length; i++) {
        let v = 0;
        for (let k = 0; k < per && k < 4; k++) v = v * 256 + b[i * per + k];
        arr[i] = per === 1 ? v : per === 2 ? v & 65535 : v >>> 0;
      }
      return arr;
    },
    randomUUID() {
      const b = rnd(16);
      b[6] = (b[6] & 15) | 64;
      b[8] = (b[8] & 63) | 128;
      const h = b.map((x) => (x < 16 ? '0' : '') + x.toString(16)).join('');
      return h.slice(0, 8) + '-' + h.slice(8, 12) + '-' + h.slice(12, 16) + '-' + h.slice(16, 20) + '-' + h.slice(20);
    },
  });

  def('TextEncoder', class TextEncoder {
    get encoding() { return 'utf-8'; }
    encode(s) {
      const bin = unescape(encodeURIComponent(String(s === undefined ? '' : s)));
      const out = new Uint8Array(bin.length);
      for (let i = 0; i < bin.length; i++) out[i] = bin.charCodeAt(i);
      return out;
    }
  });
  def('TextDecoder', class TextDecoder {
    get encoding() { return 'utf-8'; }
    decode(bytes) {
      if (!bytes) return '';
      let bin = '';
      for (let i = 0; i < bytes.length; i++) bin += String.fromCharCode(bytes[i] & 255);
      try {
        return decodeURIComponent(escape(bin));
      } catch (e) {
        return bin;
      }
    }
  });

  if (typeof G.Intl === 'undefined') {
    const group = (s) => s.replace(/\B(?=(\d{3})+(?!\d))/g, ',');
    class NumberFormat {
      constructor(locale, o) { this._o = o || {}; }
      format(n) {
        const o = this._o;
        n = Number(n);
        if (!isFinite(n)) return String(n);
        if (o.style === 'percent') n *= 100;
        const max = o.maximumFractionDigits !== undefined ? o.maximumFractionDigits : o.style === 'currency' ? 2 : 3;
        const min = o.minimumFractionDigits !== undefined ? o.minimumFractionDigits : o.style === 'currency' ? 2 : 0;
        let s = n.toFixed(Math.max(min, Math.min(max, 20)));
        if (s.indexOf('.') >= 0) {
          while (s.endsWith('0') && s.split('.')[1].length > min) s = s.slice(0, -1);
          if (s.endsWith('.')) s = s.slice(0, -1);
        }
        const parts = s.split('.');
        if (o.useGrouping !== false) parts[0] = (parts[0][0] === '-' ? '-' : '') + group(parts[0].replace('-', ''));
        s = parts.join('.');
        if (o.style === 'percent') s += '%';
        if (o.style === 'currency') s = (o.currency === 'EUR' ? '€' : o.currency === 'GBP' ? '£' : o.currency === 'JPY' ? '¥' : '$') + s;
        return s;
      }
      formatToParts(n) { return [{ type: 'literal', value: this.format(n) }]; }
      resolvedOptions() { return Object.assign({ locale: 'en-US', numberingSystem: 'latn' }, this._o); }
      static supportedLocalesOf() { return ['en-US']; }
    }
    const MONTHS = ['January', 'February', 'March', 'April', 'May', 'June', 'July', 'August', 'September', 'October', 'November', 'December'];
    class DateTimeFormat {
      constructor(locale, o) { this._o = o || {}; }
      format(d) {
        d = d === undefined ? new Date() : new Date(d);
        const o = this._o;
        if (o.month === 'long' || o.month === 'short') {
          const m = MONTHS[d.getUTCMonth()];
          return (o.month === 'short' ? m.slice(0, 3) : m) + ' ' + d.getUTCDate() + ', ' + d.getUTCFullYear();
        }
        if (o.hour !== undefined && o.year === undefined) {
          return String(d.getUTCHours()).padStart(2, '0') + ':' + String(d.getUTCMinutes()).padStart(2, '0');
        }
        return d.getUTCMonth() + 1 + '/' + d.getUTCDate() + '/' + d.getUTCFullYear();
      }
      formatToParts(d) { return [{ type: 'literal', value: this.format(d) }]; }
      resolvedOptions() { return Object.assign({ locale: 'en-US', calendar: 'gregory', numberingSystem: 'latn', timeZone: 'UTC' }, this._o); }
      static supportedLocalesOf() { return ['en-US']; }
    }
    def('Intl', {
      NumberFormat,
      DateTimeFormat,
      Collator: class Collator {
        compare(a, b) {
          a = String(a);
          b = String(b);
          return a.localeCompare(b);
        }
        resolvedOptions() { return { locale: 'en-US' }; }
      },
      PluralRules: class PluralRules {
        select(n) { return Number(n) === 1 ? 'one' : 'other'; }
        resolvedOptions() { return { locale: 'en-US' }; }
      },
      RelativeTimeFormat: class RelativeTimeFormat {
        format(v, unit) {
          unit = String(unit).replace(/s$/, '');
          const n = Math.abs(v);
          const u = n === 1 ? unit : unit + 's';
          return v < 0 ? n + ' ' + u + ' ago' : 'in ' + n + ' ' + u;
        }
      },
      getCanonicalLocales: (l) => (l === undefined ? [] : [].concat(l)),
    });
  }
})();
