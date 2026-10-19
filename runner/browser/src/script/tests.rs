//! End-to-end tests of the script engine: source in, values out.

use super::interp::{Ctrl, Interp};
use super::parser::parse_program;
use super::value::Value;

/// Runs `src` and returns String(r), or "!" + the error.
fn run(src: &str) -> String {
    let prog = match parse_program(src) {
        Ok(p) => p,
        Err(e) => return format!("!{e}"),
    };
    let mut it = Interp::new(Default::default());
    it.start_task();
    let r = it.run_program(&prog);
    let r = r.and_then(|_| {
        while let Some((f, args)) = it.microtasks.pop_front() {
            it.call(&f, Value::Undef, args)?;
        }
        Ok(())
    });
    match r {
        Ok(()) => {
            let g = Value::Obj(it.global);
            let v = it.get(&g, "r").unwrap();
            it.to_str(&v).map(|s| s.to_string()).unwrap_or_else(|_| "?".into())
        }
        Err(Ctrl::Throw(v)) => {
            let s = it.to_str(&v).map(|s| s.to_string()).unwrap_or_else(|_| "?".into());
            format!("!{s}")
        }
        Err(Ctrl::Abort(m)) => format!("!!{m}"),
        Err(e) => format!("!{e:?}"),
    }
}

fn check(cases: &[(&str, &str)]) {
    let mut bad = Vec::new();
    for (src, want) in cases {
        if std::env::var_os("JS_TRACE").is_some() { eprintln!("case: {src}"); }
        let got = run(src);
        if got != *want {
            bad.push(format!("{src}\n   want {want}\n   got  {got}"));
        }
    }
    assert!(bad.is_empty(), "\n{}", bad.join("\n"));
}

fn basics_inner() {
    check(&[
        ("var r = 1 + 2 * 3", "7"),
        ("var r = '5' * '2' + 1", "11"),
        ("var r = 0.1 + 0.2", "0.30000000000000004"),
        ("var r = 1 / 0", "Infinity"),
        ("var r = typeof null + typeof undefined + typeof 1 + typeof '' + typeof {} + typeof function(){}", "objectundefinednumberstringobjectfunction"),
        ("var r = [1, 2, 3] + ''", "1,2,3"),
        ("var r = ({}) + ''", "[object Object]"),
        ("var r = '' + (null == undefined) + (null === undefined) + (NaN == NaN) + ('1' == 1)", "truefalsefalsetrue"),
        ("var r = 7 % 3 + 2 ** 10 + (-7 >> 1) + (-1 >>> 28)", "1036"),
        ("let a = 1; { let a = 2; } var r = a", "1"),
        ("var x = 5; x += 2; x *= 3; x -= 1; var r = x", "20"),
        ("var o = null; var r = (o ?? 'd') + (o?.x ?? 'e') + (0 || 'f') + (1 && 'g')", "defg"),
        ("var r = `a${1 + 1}b${'c'}`", "a2bc"),
        ("var i = 0, s = 0; while (i < 10) { s += i++; } var r = s", "45"),
        ("var s = ''; for (var i = 0; i < 5; i++) { if (i == 1) continue; if (i == 4) break; s += i } var r = s", "023"),
        ("var s = ''; outer: for (let i of [1,2,3]) for (let j of [1,2,3]) { if (j == 2) continue outer; s += i + '' + j } var r = s", "112131"),
        ("var s = ''; for (var k in {a: 1, b: 2}) s += k; var r = s", "ab"),
        ("var r; switch (3) { case 1: r = 'a'; break; case 3: r = 'c'; case 4: r += 'd'; break; default: r = 'z' }", "cd"),
        ("var r = 'x'; do { r += 'y' } while (r.length < 4)", "xyyy"),
        ("var r = (1, 2, 3)", "3"),
        ("var r = void 0", "undefined"),
        ("var r = !!'' + '' + !!'0'", "falsetrue"),
        ("var r = parseInt('42px') + parseFloat('3.5e1x')", "77"),
        ("var r = (255).toString(16) + (0.5).toFixed(2) + (1234.5678).toFixed(1)", "ff0.501234.6"),
        ("var r = Number('  12  ') + Number('') + Number('0x10')", "28"),
        ("var r = String(-0) + String(1e21) + String(123e-20)", "01e+211.23e-18"),
        ("var r = Math.max(1, 5, 3) + Math.floor(-1.5) + Math.round(2.5) + Math.abs(-3)", "9"),
    ]);
}

#[test]
fn basics() {
    super::big_stack(basics_inner);
}

fn functions_inner() {
    check(&[
        ("function f(a, b = 10) { return a + b } var r = f(1) + f(1, 2)", "14"),
        ("function f(...xs) { return xs.length + arguments.length } var r = f(1, 2, 3)", "6"),
        ("var r = ((a, b) => a * b)(3, 4)", "12"),
        ("function mk() { var n = 0; return () => ++n } var c = mk(); c(); c(); var r = c()", "3"),
        ("var fs = []; for (let i = 0; i < 3; i++) fs.push(() => i); var r = fs.map(f => f()).join()", "0,1,2"),
        ("var r = g(); function g() { return 'hoisted' }", "hoisted"),
        ("var o = { n: 2, m() { return this.n } }; var r = o.m()", "2"),
        ("var o = { n: 2, m() { return [1].map(() => this.n)[0] } }; var r = o.m()", "2"),
        ("function F(x) { this.x = x } F.prototype.get = function () { return this.x }; var r = new F(7).get()", "7"),
        ("function f() { return this } var r = f.call(5) + 0", "5"),
        ("function f(a, b) { return this.k + a + b } var r = f.apply({k: 1}, [2, 3]) + f.bind({k: 10}, 20)(30)", "66"),
        ("var r = (function fact(n) { return n <= 1 ? 1 : n * fact(n - 1) })(10)", "3628800"),
        ("function f() { return f() } try { f() } catch (e) { var r = e instanceof RangeError }", "true"),
        ("var r = (function () { return typeof arguments })()", "object"),
        ("var f = function () {}; var r = f.name + (() => 1).name.length + function named() {}.name", "f0named"),
        ("var {a, b: {c = 3} = {}, ...rest} = {a: 1, x: 2, y: 3}; var r = a + c + Object.keys(rest).join('')", "4xy"),
        ("var [x, , y = 5, ...z] = [1, 2, undefined, 4, 5]; var r = x + y + z.length", "8"),
        ("var a = 1, b = 2; [a, b] = [b, a]; var r = '' + a + b", "21"),
        ("function f({p = 1, q} = {}) { return p + (q ?? 0) } var r = f() + f({q: 2})", "4"),
    ]);
}

#[test]
fn functions() {
    super::big_stack(functions_inner);
}

fn classes_inner() {
    check(&[
        ("class A { constructor(x) { this.x = x } get double() { return this.x * 2 } static make() { return new A(3) } } var r = A.make().double", "6"),
        ("class A { hi() { return 'A' } } class B extends A { hi() { return super.hi() + 'B' } } var r = new B().hi()", "AB"),
        ("class A { constructor(n) { this.n = n } } class B extends A { constructor() { super(5); this.m = 1 } } var b = new B(); var r = b.n + b.m + (b instanceof A)", "7"),
        ("class A { #secret = 41; reveal() { return ++this.#secret } } var r = new A().reveal()", "42"),
        ("class A { static count = 2; field = A.count * 2 } var r = new A().field", "4"),
        ("class A {} try { A() } catch (e) { var r = e.constructor.name }", "TypeError"),
        ("class E extends Error { constructor(m) { super(m); this.name = 'E' } } try { throw new E('boom') } catch (e) { var r = e.name + ':' + e.message + (e instanceof Error) }", "E:boomtrue"),
        ("class A { set v(x) { this._v = x * 2 } get v() { return this._v } } var a = new A(); a.v = 4; var r = a.v", "8"),
        ("var r = typeof class {}", "function"),
    ]);
}

#[test]
fn classes() {
    super::big_stack(classes_inner);
}

fn library_inner() {
    check(&[
        ("var r = [3, 1, 2].sort().join('-')", "1-2-3"),
        ("var r = [10, 9, 1].sort((a, b) => a - b).join()", "1,9,10"),
        ("var r = [1, 2, 3, 4].filter(x => x % 2).map(x => x * 10).reduce((a, b) => a + b, 0)", "40"),
        ("var r = [1, [2, [3, [4]]]].flat(Infinity).join()", "1,2,3,4"),
        ("var r = [1, 2, 3].includes(2) + '' + [1, 2, 3].indexOf(3) + [1, 2, 3].find(x => x > 1) + [1, 2, 3].findIndex(x => x > 5)", "true22-1"),
        ("var a = [1, 2, 3, 4, 5]; a.splice(1, 2, 'x'); var r = a.join() + '|' + a.slice(-2).join()", "1,x,4,5|4,5"),
        ("var r = Array.from({length: 3}, (_, i) => i * i).join() + Array.of(7).length + [...'abc'].reverse().join('')", "0,1,41cba"),
        ("var r = [1, 2].concat([3], 4).length + [0, 0].fill(7)[1] + [5, 1].some(x => x > 4) + [5, 1].every(x => x > 4)", "12"),
        ("var r = 'Hello'.toUpperCase() + 'abc'.charAt(1) + 'abc'.charCodeAt(0) + 'abcabc'.lastIndexOf('b')", "HELLOb974"),
        ("var r = ' x '.trim() + '|' + 'ab'.padStart(4, '-') + '|' + 'ab'.repeat(2) + '|' + 'a-b-c'.split('-').join('+')", "x|--ab|abab|a+b+c"),
        ("var r = 'abc'.slice(-2) + 'abc'.substring(2, 0) + 'abcdef'.substr(1, 2) + 'abc'.at(-1)", "bcabbcc"),
        ("var r = 'a1b22c333'.replace(/\\d+/g, m => '<' + m.length + '>')", "a<1>b<2>c<3>"),
        ("var r = '2024-05-06'.replace(/(\\d+)-(\\d+)-(\\d+)/, '$3/$2/$1')", "06/05/2024"),
        ("var m = /(?<y>\\d{4})-(?<m>\\d\\d)/.exec('on 2024-05'); var r = m.index + m.groups.y + m[2]", "3202405"),
        ("var r = [...'a1b2'.matchAll(/[a-z](\\d)/g)].map(m => m[1]).join()", "1,2"),
        ("var r = 'A,b;C'.split(/[,;]/).length + ''.split(',').length + 'abc'.split('').length", "7"),
        ("var re = /o/g; var s = 'foo'; re.exec(s); var r = re.lastIndex + /^a.c$/i.test('ABC')", "3"),
        ("var r = JSON.stringify({a: [1, 'x', null, true], b: {c: undefined}, d: 'q\"'})", "{\"a\":[1,\"x\",null,true],\"b\":{},\"d\":\"q\\\"\"}"),
        ("var r = JSON.parse('{\"a\": [1, 2, {\"b\": \"\\\\u0041\"}]}').a[2].b", "A"),
        ("var r = JSON.stringify({a: 1, b: [2]}, null, 2)", "{\n  \"a\": 1,\n  \"b\": [\n    2\n  ]\n}"),
        ("try { JSON.parse('{bad') } catch (e) { var r = e.name }", "SyntaxError"),
        ("var o = {a: 1}; o.self = o; try { JSON.stringify(o) } catch (e) { var r = e.name }", "TypeError"),
        ("var r = Object.entries({a: 1, b: 2}).map(([k, v]) => k + v).join() + Object.assign({}, {x: 1}, {y: 2}).y", "a1,b22"),
        ("var o = Object.freeze({a: 1}); o.a = 2; var r = o.a + Object.isFrozen(o)", "2"),
        ("var o = {}; Object.defineProperty(o, 'x', {get() { return 9 }}); var r = o.x + Object.keys(o).length", "9"),
        ("var r = Object.getPrototypeOf([]) === Array.prototype", "true"),
        ("var p = {greet() { return 'hi ' + this.n }}; var o = Object.create(p); o.n = 'bo'; var r = o.greet() + ('greet' in o) + o.hasOwnProperty('greet')", "hi botruefalse"),
        ("var m = new Map([[1, 'a']]); m.set('k', 'v'); var r = m.get(1) + m.size + m.has('k') + [...m.keys()].join()", "a2true1,k"),
        ("var s = new Set([1, 2, 2, 3]); s.delete(1); var r = s.size + [...s].join()", "22,3"),
        ("var r = new Date(Date.UTC(2020, 1, 29, 12)).toISOString()", "2020-02-29T12:00:00.000Z"),
        ("var r = new Date('2021-03-04T05:06:07Z').getTime()", "1614834367000"),
        ("var r = encodeURIComponent('a b&c/é') + decodeURIComponent('%E2%9C%93')", "a%20b%26c%2F%C3%A9✓"),
        ("var r = Number.isInteger(5) + '' + Number.isNaN('x') + isNaN('x') + Number.MAX_SAFE_INTEGER", "truefalsetrue9007199254740991"),
        ("var r = [1, 2, 3].entries().next().value.join() + typeof Symbol.iterator", "0,1string"),
        ("var o = {*[Symbol.iterator]() {}}", "!SyntaxError: generators are not supported (line 1)"),
        ("var o = {[Symbol.iterator]() { var i = 0; return {next: () => ({done: i >= 2, value: i++})} }}; var r = [...o].join()", "0,1"),
        ("var r = structuredClone({a: [1, {b: 2}]}).a[1].b", "2"),
    ]);
}

#[test]
fn library() {
    super::big_stack(library_inner);
}

fn promises_inner() {
    check(&[
        ("var r = 'sync'; Promise.resolve(1).then(v => { r = 'then' + v })", "then1"),
        ("var r = ''; Promise.reject(new Error('x')).catch(e => r = e.message).finally(() => r += '!')", "x!"),
        ("var r = ''; async function f() { return 5 } f().then(v => r = v * 2)", "10"),
        ("var r; async function f() { var a = await 2; var b = await Promise.resolve(3); return a * b } f().then(v => r = v)", "6"),
        ("var r; async function f() { try { await Promise.reject('no') } catch (e) { return 'caught ' + e } } f().then(v => r = v)", "caught no"),
        ("var r; Promise.all([1, Promise.resolve(2), new Promise(res => res(3))]).then(v => r = v.join())", "1,2,3"),
        ("var r; Promise.allSettled([Promise.reject(1), 2]).then(v => r = v.map(x => x.status).join())", "rejected,fulfilled"),
        ("var r; Promise.race([new Promise(() => {}), Promise.resolve('fast')]).then(v => r = v)", "fast"),
        ("var log = []; Promise.resolve().then(() => log.push(2)); log.push(1); queueMicrotask(() => log.push(3)); var r; Promise.resolve().then(() => r = log.join())", "1,2,3"),
        ("var r; async function f() { await new Promise(() => {}) } f().catch(e => r = e.name)", "TypeError"),
    ]);
}

#[test]
fn promises() {
    super::big_stack(promises_inner);
}

fn errors_and_limits_inner() {
    check(&[
        ("undefinedThing", "!ReferenceError: undefinedThing is not defined"),
        ("null.x", "!TypeError: Cannot read properties of null (reading 'x')"),
        ("var r; try { (void 0)() } catch (e) { r = e instanceof TypeError }", "true"),
        ("const c = 1; try { c = 2 } catch (e) { var r = e.name }", "TypeError"),
        ("var r; try { x; let x = 1 } catch (e) { r = e.name }", "ReferenceError"),
        ("var r = 'a'; try { throw 'b' } catch (e) { r += e } finally { r += 'c' }", "abc"),
        ("var r = (function () { try { return 'try' } finally { r = 'fin' } })()", "try"),
        ("while (true) {}", "!!the script ran too long"),
        ("var s = 'x'; while (true) s += s", "!RangeError: Invalid string length"),
        ("var a = []; while (true) a.push({})", "!!the script used too much memory"),
        ("var r; try { /(a+)+$/.test('a'.repeat(40) + 'b') } catch (e) { r = e.name }", "RangeError"),
        ("try { new Function('return 1') } catch (e) { var r = e.name }", "TypeError"),
        ("var r = typeof eval", "undefined"),
    ]);
}

#[test]
fn errors_and_limits() {
    super::big_stack(errors_and_limits_inner);
}

/// The deepest things a script can do must fit the browser thread's stack
/// with room left for layout (the thread has 32 MB; scripts get 24).
fn deep_inner() {
    let n = super::interp::MAX_DEPTH;
    check(&[
        ("function f(n) { return n ? f(n - 1) + 1 : 0 } var r; try { f(1e6) } catch (e) { r = e.name }", "RangeError"),
        ("var o = { m(n) { return n ? this.m(n - 1) : 0 } }; var r; try { o.m(1e6) } catch (e) { r = e.name }", "RangeError"),
        ("function f(n) { return n ? [n].map(x => f(x - 1))[0] : 0 } var r; try { f(1e6) } catch (e) { r = e.name }", "RangeError"),
        ("class A { constructor(n) { if (n) new A(n - 1) } } var r; try { new A(1e6) } catch (e) { r = e.name }", "RangeError"),
        ("var o = { get g() { return this.g } }; var r; try { o.g } catch (e) { r = e.name }", "RangeError"),
        ("var o = { toString() { return '' + this } }; var r; try { '' + o } catch (e) { r = e.name }", "RangeError"),
        ("var a = []; for (var i = 0; i < 1e5; i++) a = [a]; var r; try { JSON.stringify(a) } catch (e) { r = e.name }", "RangeError"),
        ("var a = []; for (var i = 0; i < 1e5; i++) a = [a]; var r; try { String(a) } catch (e) { r = e.name }", "RangeError"),
        ("var a = []; for (var i = 0; i < 1e5; i++) a = [a]; var r; try { a.flat(Infinity) } catch (e) { r = e.name }", "RangeError"),
        ("async function f(n) { return n ? await f(n - 1) : 0 } var r; f(1e6).catch(e => r = e.name)", "RangeError"),
        ("function f(n) { try { return n ? f(n - 1) : 0 } finally { } } var r; try { f(1e6) } catch (e) { r = e.name }", "RangeError"),
    ]);
    let nested = format!("var r = {}1{}", "(".repeat(n / 8), ")".repeat(n / 8));
    assert_eq!(run(&nested), "1");
    let too_deep = format!("var r = {}1{}", "[".repeat(5000), "]".repeat(5000));
    assert!(run(&too_deep).starts_with("!SyntaxError"));
    let json = format!("var r = JSON.parse('{}1{}').length", "[".repeat(5000), "]".repeat(5000));
    assert_eq!(run(&json), "!SyntaxError: JSON nested too deeply");
}

#[test]
fn deep() {
    std::thread::Builder::new().stack_size(24 << 20).spawn(deep_inner).unwrap().join().unwrap();
}

// ---- pages ------------------------------------------------------------------------------

use super::{PageScript, Runtime, Setup, Source};
use crate::dom::Document;
use crate::paint::FormState;

fn fake_fetch(r: &crate::net::Request) -> Result<crate::net::Response, crate::net::FetchError> {
    let path = r.url.path().to_string();
    let (status, body) = match path.as_str() {
        "/api/data.json" => (200, "{\"items\":[1,2,3],\"name\":\"æther\"}".to_string()),
        "/api/echo" => (200, format!("{} {}", r.method.as_str(), String::from_utf8_lossy(&r.body))),
        "/late.js" => (200, "window.lateRan = (window.lateRan || 0) + 1; document.body.dataset.late = 'yes';".to_string()),
        _ => (404, "not found".to_string()),
    };
    Ok(crate::net::Response { url: r.url.clone(), status, headers: vec![("Content-Type".into(), "text/plain".into())], body: body.into_bytes(), secure: true })
}

struct TestPage {
    rt: Runtime,
    doc: Document,
    form: FormState,
}

impl TestPage {
    fn new(html: &str) -> TestPage {
        let mut doc = crate::html::parse(html);
        let mut form = crate::page::initial_form(&doc);
        let url = crate::net::Url::parse("https://example.test/dir/page.html").unwrap();
        let scripts: Vec<PageScript> = doc.find_all("script").into_iter().map(|n| PageScript { node: n, source: Source::Inline(doc.text_content(n)) }).collect();
        let mut rt = Runtime::new(Setup { url: url.clone(), base: Some(url), third_party: false, fetcher: Some(&fake_fetch), storage: Default::default(), viewport: (800.0, 600.0) });
        rt.run_page(&mut doc, &mut form, &scripts);
        rt.settle(&mut doc, &mut form, 5000.0);
        TestPage { rt, doc, form }
    }

    fn r(&mut self, src: &str) -> String {
        let out = self.rt.eval_r(&mut self.doc, &mut self.form, src);
        self.rt.settle(&mut self.doc, &mut self.form, 5000.0);
        out
    }

    fn after(&mut self, src: &str) -> String {
        self.rt.eval_r(&mut self.doc, &mut self.form, src);
        self.rt.settle(&mut self.doc, &mut self.form, 5000.0);
        self.r("")
    }
}

fn dom_basics_inner() {
    let mut p = TestPage::new("<!doctype html><title>T</title><body><div id=a class='x y'><p>one</p><p>two</p></div><ul></ul><script>var r0 = document.querySelectorAll('p').length;</script>");
    assert_eq!(p.r("var r = r0"), "2");
    assert_eq!(p.r("var r = document.getElementById('a').className + '|' + document.title"), "x y|T");
    assert_eq!(p.r("var r = document.querySelector('#a > p:last-child').textContent"), "two");
    assert_eq!(p.r("var ul = document.querySelector('ul'); for (var i = 0; i < 3; i++) { var li = document.createElement('li'); li.textContent = 'item ' + i; ul.appendChild(li) } var r = ul.children.length + ul.innerHTML"), "3<li>item 0</li><li>item 1</li><li>item 2</li>");
    assert_eq!(p.r("var a = document.getElementById('a'); a.classList.add('z'); a.classList.remove('x'); var r = a.className + a.classList.contains('y')"), "y ztrue");
    assert_eq!(p.r("var a = document.getElementById('a'); a.style.backgroundColor = 'red'; a.style.marginTop = '4px'; var r = a.getAttribute('style') + '|' + a.style.backgroundColor"), "background-color: red; margin-top: 4px;|red");
    assert_eq!(p.r("var a = document.getElementById('a'); a.dataset.userId = '7'; var r = a.getAttribute('data-user-id') + Object.keys(a.dataset).join()"), "7userId");
    assert_eq!(p.r("var d = document.createElement('div'); d.innerHTML = '<b>bold</b> &amp; <i>it</i>'; var r = d.childNodes.length + d.textContent + d.firstChild.tagName"), "3bold & itB");
    assert_eq!(p.r("var r = document.body.contains(document.querySelector('p')) + '' + (document.querySelector('p').parentNode === document.getElementById('a'))"), "truetrue");
    assert_eq!(p.r("var p1 = document.querySelector('p'); p1.remove(); var r = document.querySelectorAll('#a p').length + String(p1.isConnected)"), "1false");
    assert_eq!(p.r("var a = document.getElementById('a'); a.insertAdjacentHTML('beforeend', '<span>s</span>'); var r = a.lastElementChild.outerHTML"), "<span>s</span>");
    assert_eq!(p.r("var f = document.createDocumentFragment(); f.append('x', document.createElement('hr')); document.body.append(f); var r = document.body.lastChild.nodeName + f.childNodes.length"), "HR0");
    assert_eq!(p.r("var r = document.querySelector('li:nth-child(2)').closest('ul').tagName + document.querySelector('li').matches('ul > li')"), "ULtrue");
    assert_eq!(p.r("try { document.querySelector('a[') } catch (e) { var r = e.name }"), "SyntaxError");
    assert_eq!(p.r("document.title = 'New'; var r = document.title + document.querySelector('title').textContent"), "NewNew");
    assert_eq!(p.r("var r = typeof window.document + (window === self) + (globalThis.window === window) + location.hostname + location.pathname"), "objecttruetrueexample.test/dir/page.html");
    assert_eq!(p.r("var c = document.getElementById('a').cloneNode(true); c.id = 'b'; document.body.appendChild(c); var r = document.querySelectorAll('[id]').length"), "2");
    assert!(p.doc.title() == "New");
}

#[test]
fn dom_basics() {
    super::big_stack(dom_basics_inner);
}

fn events_inner() {
    let mut p = TestPage::new(
        "<body><div id=outer><button id=b onclick=\"window.inline = (window.inline || 0) + 1\">Go</button></div>\
         <form id=f action=/search><input id=q name=q value=hello><input type=checkbox id=c name=c></form>\
         <script>var log = []; document.getElementById('outer').addEventListener('click', function (e) { log.push('outer:' + e.target.id + ':' + e.eventPhase) });\
         document.addEventListener('click', function (e) { log.push('doc') }, true);\
         window.addEventListener('load', function () { log.push('load') });\
         document.addEventListener('DOMContentLoaded', function () { log.push('ready') });</script>",
    );
    assert_eq!(p.r("var r = log.join()"), "ready,load");
    assert_eq!(p.r("log = []; document.getElementById('b').click(); var r = log.join() + '|' + inline"), "doc,outer:b:3|1");
    assert_eq!(p.r("log = []; var b = document.getElementById('b'); b.addEventListener('click', function (e) { e.stopPropagation(); log.push('b') }); b.click(); var r = log.join()"), "doc,b");
    assert_eq!(p.r("var n = 0; var h = function () { n++ }; var b = document.getElementById('b'); b.addEventListener('x', h, {once: true}); b.dispatchEvent(new Event('x')); b.dispatchEvent(new Event('x')); var r = n"), "1");
    assert_eq!(p.r("var e = new CustomEvent('hi', {detail: {a: 1}, cancelable: true}); var got; window.addEventListener('hi', function (ev) { got = ev.detail.a; ev.preventDefault() }); var res = window.dispatchEvent(e); var r = got + '' + res + e.defaultPrevented"), "1falsetrue");
    // Form values, checkboxes and submit.
    assert_eq!(p.r("var q = document.getElementById('q'); var r = q.value; q.value = 'world'"), "hello");
    assert_eq!(p.form.values.get(&crate::script::dom::get_element_by_id(&p.doc, "q").unwrap()).map(|s| s.as_str()), Some("world"));
    assert_eq!(p.r("var c = document.getElementById('c'); var changes = 0; c.addEventListener('change', function () { changes++ }); c.click(); var r = c.checked + '' + changes"), "true1");
    assert_eq!(p.r("var f = document.getElementById('f'); f.addEventListener('submit', function (e) { e.preventDefault(); window.submitted = new FormData(f).get('q') }); f.requestSubmit(); var r = window.submitted"), "world");
    let fx = p.rt.take_effects();
    assert!(fx.submit.is_none());
    assert_eq!(p.r("document.getElementById('f').submit(); var r = 1"), "1");
    assert!(p.rt.take_effects().submit.is_some());
}

#[test]
fn events() {
    super::big_stack(events_inner);
}

fn timers_and_fetch_inner() {
    let mut p = TestPage::new("<body><script>var ticks = []; setTimeout(function () { ticks.push('a') }, 50); setTimeout(function () { ticks.push('b') }, 10); var iv = setInterval(function () { ticks.push('i'); if (ticks.length > 4) clearInterval(iv) }, 20);</script>");
    assert_eq!(p.r("var r = ticks.join()"), "b,i,i,a,i");
    assert_eq!(p.after("var r; fetch('/api/data.json').then(function (res) { return res.json() }).then(function (d) { r = d.items.length + d.name })"), "3æther");
    assert_eq!(p.after("var r; fetch('https://other.test/x').catch(function (e) { r = e.name + ': ' + e.message })"), "TypeError: Failed to fetch: requests to other sites are blocked (https://other.test)");
    assert_eq!(p.after("var r; (async function () { var res = await fetch('/api/echo', {method: 'POST', body: new URLSearchParams({a: 'b c'})}); r = res.status + ' ' + await res.text() })()"), "200 POST a=b+c");
    assert_eq!(p.after("var r; var x = new XMLHttpRequest(); x.open('GET', '/api/data.json'); x.onload = function () { r = x.status + ':' + JSON.parse(x.responseText).name }; x.send()"), "200:æther");
    assert_eq!(p.after("var r; var x = new XMLHttpRequest(); x.open('GET', '/nope'); x.onreadystatechange = function () { if (x.readyState === 4) r = x.status }; x.send()"), "404");
    // A script added to the page loads and runs once.
    assert_eq!(p.after("var r; var s = document.createElement('script'); s.src = '/late.js'; s.onload = function () { r = lateRan + document.body.dataset.late }; document.head.appendChild(s)"), "1yes");
    assert_eq!(p.after("var r; var s = document.createElement('script'); s.src = 'https://tracker.other.test/t.js'; s.onerror = function () { r = 'blocked' }; document.head.appendChild(s)"), "blocked");
    assert_eq!(p.r("var s = document.createElement('script'); s.textContent = 'window.inl = 5'; document.body.appendChild(s); var r = window.inl"), "5");
}

#[test]
fn timers_and_fetch() {
    super::big_stack(timers_and_fetch_inner);
}

fn window_inner() {
    let mut p = TestPage::new("<body><a id=l href='/next?x=1#frag'>next</a>");
    assert_eq!(p.r("var u = new URL('../a/b?c=d#e', location.href); var r = u.href + '|' + u.searchParams.get('c')"), "https://example.test/a/b?c=d#e|d");
    assert_eq!(p.r("var u = new URL('https://x.test/p'); u.searchParams.set('q', 'a b'); var r = u.href"), "https://x.test/p?q=a+b");
    assert_eq!(p.r("var l = document.getElementById('l'); var r = l.href + '|' + l.pathname + l.search + l.hash"), "https://example.test/next?x=1#frag|/next?x=1#frag");
    assert_eq!(p.r("localStorage.setItem('k', 'v'); var r = localStorage.getItem('k') + localStorage.length + sessionStorage.getItem('k')"), "v1null");
    assert_eq!(p.r("document.cookie = 'a=1'; document.cookie = 'b=2; path=/'; var r = document.cookie"), "a=1; b=2");
    assert_eq!(p.r("var r = btoa('hello') + atob('aGk=') + typeof crypto.randomUUID() + crypto.getRandomValues(new Uint8Array(4)).length"), "aGVsbG8=histring4");
    assert_eq!(p.r("var r = new TextDecoder().decode(new TextEncoder().encode('é✓'))"), "é✓");
    assert_eq!(p.r("var r = matchMedia('(min-width: 600px)').matches + '' + matchMedia('(max-width: 600px)').matches"), "truefalse");
    assert_eq!(p.r("var r = new Intl.NumberFormat('en-US').format(1234567.891)"), "1,234,567.891");
    assert_eq!(p.r("try { Function('return 1') } catch (e) { var r = typeof eval + e.name }"), "undefinedTypeError");
    assert_eq!(p.r("var r = setTimeout('alert(1)', 0)"), "0");
    assert_eq!(p.r("history.pushState({}, '', '/other'); var r = location.pathname"), "/other");
    let fx = p.rt.take_effects();
    assert_eq!(fx.url_changed.map(|u| u.0.as_str().to_string()), Some("https://example.test/other".into()));
    assert_eq!(p.r("try { history.pushState({}, '', 'https://evil.test/') } catch (e) { var r = e.message }"), "SecurityError: history can only change to an address on the same site");
    assert_eq!(p.r("location.href = 'page2.html'; var r = 1"), "1");
    assert_eq!(p.rt.take_effects().nav, Some(super::NavReq::Go { url: "https://example.test/page2.html".into(), replace: false }));
    assert_eq!(p.r("var r = window.open('https://x.test') === null"), "true");
    assert_eq!(p.rt.take_effects().alerts.len(), 1);
}

#[test]
fn window() {
    super::big_stack(window_inner);
}
