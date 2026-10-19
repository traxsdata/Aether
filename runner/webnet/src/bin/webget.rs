//! webget - fetch a URL with webnet and print the status, headers and body size.
//!
//!     webget [options] URL
//!       --head              HEAD instead of GET
//!       --post DATA         POST DATA as application/x-www-form-urlencoded
//!       --allow-http        permit plain http (otherwise refused)
//!       --body              print the body too
//!       --max BYTES         body size limit (default 8 MiB)
//!       --ca-file PEM       ALSO trust the roots in PEM (testing only, e.g.
//!                           an intercepting proxy's CA)
//!       --only-ca-file PEM  trust ONLY the roots in PEM (local tests)
//!       --connect-to ADDR   connect to ADDR instead of resolving the host
//!                           (host development only; ignored on æther)
//!       --proxy             tunnel through $HTTPS_PROXY with CONNECT (host
//!                           development only; ignored on æther)
//!
//! The extra-root and proxy options exist only in this test tool: the
//! library's `fetch` never uses them.

use std::process::exit;
use webnet::{fetch_with, roots::RootStore, Config, Method, Request, Url};

fn usage() -> ! {
    eprintln!(
        "usage: webget [--head] [--post DATA] [--allow-http] [--body] [--max BYTES] \
         [--ca-file PEM] [--only-ca-file PEM] [--connect-to ADDR] [--proxy] URL"
    );
    exit(2)
}

fn load_pem(path: &str, store: &mut RootStore) {
    let text = std::fs::read_to_string(path).unwrap_or_else(|e| {
        eprintln!("webget: {path}: {e}");
        exit(2)
    });
    let n = store.add_pem(&text);
    eprintln!("webget: trusting {n} extra root(s) from {path}");
}

fn main() {
    let mut args = std::env::args().skip(1);
    let mut url = None;
    let mut method = Method::Get;
    let mut body = Vec::new();
    let mut allow_http = false;
    let mut show_body = false;
    let mut max = webnet::DEFAULT_MAX_BYTES;
    let mut cfg = Config::default();
    let mut roots: Option<RootStore> = None;
    while let Some(a) = args.next() {
        match a.as_str() {
            "--head" => method = Method::Head,
            "--post" => {
                method = Method::Post;
                body = args.next().unwrap_or_else(|| usage()).into_bytes();
            }
            "--allow-http" => allow_http = true,
            "--body" => show_body = true,
            "--max" => max = args.next().and_then(|v| v.parse().ok()).unwrap_or_else(|| usage()),
            "--ca-file" => {
                let p = args.next().unwrap_or_else(|| usage());
                let s = roots.get_or_insert_with(RootStore::builtin);
                load_pem(&p, s);
            }
            "--only-ca-file" => {
                let p = args.next().unwrap_or_else(|| usage());
                let mut s = RootStore::empty();
                load_pem(&p, &mut s);
                roots = Some(s);
            }
            "--connect-to" => cfg.connect_to = Some(args.next().unwrap_or_else(|| usage())),
            "--proxy" => {
                let p = std::env::var("HTTPS_PROXY").or_else(|_| std::env::var("https_proxy")).unwrap_or_else(|_| {
                    eprintln!("webget: --proxy needs HTTPS_PROXY");
                    exit(2)
                });
                let hp = p.trim_start_matches("http://").trim_end_matches('/');
                let (h, port) = hp.rsplit_once(':').unwrap_or((hp, "80"));
                cfg.proxy = Some((h.to_string(), port.parse().unwrap_or(80)));
            }
            s if s.starts_with("--") => usage(),
            s => url = Some(s.to_string()),
        }
    }
    let url = url.unwrap_or_else(|| usage());
    let url = Url::from_user_input(&url).unwrap_or_else(|e| {
        eprintln!("webget: {e}");
        exit(1)
    });
    cfg.roots = roots;
    let req = Request { url, method, body, allow_http, max_bytes: max };
    let t = std::time::Instant::now();
    match fetch_with(&cfg, &req) {
        Ok(r) => {
            let sec = if r.secure { "secure" } else { "NOT SECURE" };
            println!("{} {}  ({sec}, {} ms)", r.status, r.url, t.elapsed().as_millis());
            for (k, v) in &r.headers {
                println!("{k}: {v}");
            }
            println!("-- body: {} bytes", r.body.len());
            if show_body {
                println!("{}", String::from_utf8_lossy(&r.body));
            }
        }
        Err(e) => {
            eprintln!("webget: {e}");
            exit(1)
        }
    }
}
