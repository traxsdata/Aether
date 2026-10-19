//! Browser - a small, secure web browser for æther. Its own script engine
//! with tight limits, no plugins, nothing kept on disk. See docs/BROWSER.md.
//!
//!   browser [address]
//!
//! On æther (AETHER=1) it opens a window; on a Linux host it runs headless,
//! driven by AETHER_SCRIPT (see aether-host's headless backend). On a host,
//! BROWSER_TEST_ROOT=<dir> serves https://test.aether/ from that folder and
//! refuses every other address (tests and screenshots without a network).

fn main() {
    let start = std::env::args().nth(1);
    let mut fetcher = browser::page::default_fetcher();
    if !aether_host::on_aether() {
        if let Some(root) = std::env::var_os("BROWSER_TEST_ROOT") {
            browser::testserve::set_root(root.into());
            fetcher = &browser::testserve::fetch;
        }
    }
    // Page layout recurses (bounded); give it room.
    let t = std::thread::Builder::new().name("browser".into()).stack_size(32 << 20).spawn(move || {
        let backend = aether_host::open();
        if let Err(e) = browser::ui::run(backend, start, fetcher) {
            eprintln!("browser: {e}");
            std::process::exit(1);
        }
    });
    match t {
        Ok(t) => {
            if t.join().is_err() {
                std::process::exit(101);
            }
        }
        Err(e) => {
            eprintln!("browser: can't start: {e}");
            std::process::exit(1);
        }
    }
}
