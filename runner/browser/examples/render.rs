//! render - lays an HTML file out and writes it as a PNG, for looking at the
//! engine's output without the browser's chrome.
//!
//!   cargo run -p browser --example render -- page.html out.png [width] [height]
//!
//! Linked stylesheets and scripts are read from the HTML file's folder
//! (served as https://test.aether/ by browser::testserve, without a
//! network). The page's scripts run (SCRIPTS=0 turns them off), with
//! their timers run ahead by up to two seconds; errors go to stderr.

use std::sync::atomic::AtomicBool;

use browser::font::Cache;
use browser::net::{Request, Url};
use browser::page;
use browser::paint::{paint_page, Canvas};

fn main() {
    let args: Vec<String> = std::env::args().collect();
    if args.len() < 3 {
        eprintln!("usage: render <page.html> <out.png> [width] [max height]");
        std::process::exit(2);
    }
    let path = std::path::Path::new(&args[1]);
    let w: u32 = args.get(3).and_then(|s| s.parse().ok()).unwrap_or(900);
    let max_h: u32 = args.get(4).and_then(|s| s.parse().ok()).unwrap_or(4000);
    let dir = path.parent().unwrap_or(std::path::Path::new(".")).canonicalize().unwrap();
    browser::testserve::set_root(dir);
    let name = path.file_name().unwrap().to_string_lossy().to_string();
    let out = args[2].clone();
    let t = std::thread::Builder::new()
        .stack_size(64 << 20)
        .spawn(move || {
            let url = Url::parse(&format!("https://test.aether/{name}")).unwrap();
            let t0 = std::time::Instant::now();
            let mut policy = browser::script::Policy::default();
            if std::env::var("SCRIPTS").is_ok_and(|v| v == "0") {
                policy.set(&url, browser::script::Mode::Off);
            }
            let mut l = page::load(Request::get(url.clone()), None, &browser::testserve::fetch, &AtomicBool::new(false), &policy).unwrap_or_else(|e| panic!("{e}"));
            for b in &l.blocked {
                eprintln!("blocked: {b}");
            }
            let mut form = page::initial_form(&l.doc);
            if l.script_mode.runs() {
                let mut rt = browser::script::Runtime::new(browser::script::Setup {
                    url,
                    base: l.base.clone(),
                    third_party: false,
                    fetcher: Some(&browser::testserve::fetch),
                    storage: Default::default(),
                    viewport: (w as f32, 700.0),
                });
                let scripts = std::mem::take(&mut l.scripts);
                rt.run_page(&mut l.doc, &mut form, &scripts);
                rt.settle(&mut l.doc, &mut form, 2000.0);
                for e in rt.console() {
                    eprintln!("console: {e}");
                }
                l.title = l.doc.title();
            }
            let pg = page::lay_out(&l, w as f32, 700.0);
            let t1 = t0.elapsed();
            let h = (pg.height.ceil() as u32).clamp(100, max_h);
            let mut px = vec![0u32; (w * h) as usize];
            let mut cache = Cache::default();
            {
                let mut cv = Canvas::new(&mut px, w, h, w);
                paint_page(&mut cv, &mut cache, &pg, &l.images, 0, 0, 0.0, &form);
            }
            eprintln!("title {:?}, {} items, {}x{} (page {}), layout {:?}, total {:?}", l.title, pg.items.len(), w, h, pg.height, t1, t0.elapsed());
            std::fs::write(&out, aether_host::png::encode_xrgb(&px, w, h, w)).unwrap();
        })
        .unwrap();
    if t.join().is_err() {
        std::process::exit(1);
    }
}
