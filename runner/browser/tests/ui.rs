//! Browser behaviour through the window's event handling: navigation,
//! the http warning, history, links, forms, scrolling and focus. Pages come
//! from browser::testserve (https://test.aether/ is runner/browser/testpages;
//! no network is used).

use aether_host::{abi, Event, Mouse};
use browser::ui::State;

fn setup() -> State {
    browser::testserve::set_root(concat!(env!("CARGO_MANIFEST_DIR"), "/testpages").into());
    let mut st = State::new(1000, 700, true);
    st.set_fetcher(&browser::testserve::fetch);
    st
}

/// Runs `f` on a thread with a large stack, as the browser does.
fn big<F: FnOnce() + Send + 'static>(f: F) {
    std::thread::Builder::new().stack_size(64 << 20).spawn(f).unwrap().join().unwrap();
}

fn key(st: &mut State, k: u32, mods: u32) {
    assert!(st.handle(Event::Key { key: k, mods }));
}

fn type_text(st: &mut State, s: &str) {
    for c in s.chars() {
        key(st, c as u32, 0);
    }
}

fn click(st: &mut State, (x, y): (i32, i32)) {
    let m = Mouse { x, y, buttons: 1, mods: 0 };
    st.handle(Event::MouseDown(m));
    st.handle(Event::MouseUp(Mouse { buttons: 0, ..m }));
}

fn go(st: &mut State, addr: &str) {
    key(st, 'l' as u32, abi::MOD_CTRL);
    type_text(st, addr);
    key(st, '\n' as u32, 0);
}

#[test]
fn bare_names_mean_https() {
    big(|| {
        let mut st = setup();
        assert_eq!(st.address(), "");
        go(&mut st, "test.aether");
        assert_eq!(st.address(), "https://test.aether/");
        assert!(st.page_text().contains("It works"), "{}", st.page_text());
        assert_eq!(st.window_title(), "test.aether");
        assert!(st.is_secure());
    });
}

#[test]
fn http_needs_consent_and_back_works() {
    big(|| {
        let mut st = setup();
        go(&mut st, "test.aether/article.html");
        assert!(st.page_text().contains("Lighthouse"));
        go(&mut st, "http://test.aether/listings.html");
        assert!(st.is_interstitial());
        assert!(st.page_text().contains("isn't private"));
        // "Continue over http" is the second focusable link.
        key(&mut st, '\t' as u32, 0);
        key(&mut st, '\t' as u32, 0);
        key(&mut st, '\n' as u32, 0);
        assert!(!st.is_interstitial());
        assert!(st.page_text().contains("Branches"));
        assert!(!st.is_secure());
        assert_eq!(st.address(), "http://test.aether/listings.html");
        // Back to the article, forward again (consent is remembered for the host).
        key(&mut st, abi::KEY_LEFT, abi::MOD_ALT);
        assert!(st.page_text().contains("Lighthouse"));
        key(&mut st, abi::KEY_RIGHT, abi::MOD_ALT);
        assert!(st.page_text().contains("Branches"));
    });
}

#[test]
fn errors_show_the_fetch_error() {
    big(|| {
        let mut st = setup();
        go(&mut st, "https://nowhere.example/");
        let t = st.page_text();
        assert!(t.contains("Can't open this page"), "{t}");
        assert!(t.contains("only test.aether is served"), "{t}");
        go(&mut st, "not a valid address");
        assert!(st.page_text().contains("Can't open this page"));
    });
}

#[test]
fn hostile_page_cannot_escape() {
    big(|| {
        let mut st = setup();
        go(&mut st, "test.aether/hostile.html");
        let t = st.page_text();
        assert!(t.contains("A page that tries things"), "{t}");
        assert!(!t.contains("noscript"), "noscript content is hidden while scripts run: {t}");
        assert!(!t.contains("You should not see this"));
        assert!(!t.contains("alert"));
        // The endless loop was stopped, the other site's script never loaded,
        // and the missing function is just an error.
        let errors = st.script_errors().join("\n");
        assert!(errors.contains("ran too long"), "{errors}");
        assert!(errors.contains("steal is not defined"), "{errors}");
        // A javascript: link runs on the page; its alert only reaches the status line.
        let p = st.find_link("javascript:").unwrap();
        click(&mut st, p);
        assert!(st.status().starts_with("The page says"), "{}", st.status());
        assert_eq!(st.address(), "https://test.aether/hostile.html");
        // A page can't use the warning page's internal "continue" action.
        let p = st.find_link("about:continue").unwrap();
        click(&mut st, p);
        assert_eq!(st.address(), "https://test.aether/hostile.html");
        // Nor open other internal pages.
        let p = st.find_link("about:config").unwrap();
        click(&mut st, p);
        assert_eq!(st.address(), "https://test.aether/hostile.html");
        // The in-page link scrolls.
        let p = st.find_link("#bottom").unwrap();
        st.handle(Event::Scroll { dx: 0, dy: 100, mods: 0 });
        let before = st.scroll();
        click(&mut st, p);
        assert!(st.scroll() >= before);
        assert_eq!(st.address(), "https://test.aether/hostile.html");
    });
}

#[test]
fn links_and_forms() {
    big(|| {
        let mut st = setup();
        go(&mut st, "test.aether/listings.html");
        let p = st.find_link("central.html").unwrap();
        click(&mut st, p);
        assert_eq!(st.address(), "https://test.aether/central.html");
        assert!(st.page_text().contains("404"));
        key(&mut st, 8, 0); // Backspace: back
        assert_eq!(st.address(), "https://test.aether/listings.html");
        // Tab to the search field, type, press Enter.
        for _ in 0..40 {
            key(&mut st, '\t' as u32, 0);
            if st.status().is_empty() {
                break; // a field (no status text)
            }
        }
        type_text(&mut st, "lighthouse keepers");
        key(&mut st, '\n' as u32, 0);
        assert_eq!(st.address(), "https://test.aether/search.html?q=lighthouse+keepers&where=All+branches");
    });
}

#[test]
fn scrolling() {
    big(|| {
        let mut st = setup();
        go(&mut st, "test.aether/article.html");
        assert_eq!(st.scroll(), 0.0);
        key(&mut st, abi::KEY_PGDN, 0);
        let one = st.scroll();
        assert!(one > 400.0, "{one}");
        key(&mut st, ' ' as u32, 0);
        assert!(st.scroll() > one);
        key(&mut st, abi::KEY_END, 0);
        let end = st.scroll();
        key(&mut st, abi::KEY_DOWN, 0);
        assert_eq!(st.scroll(), end);
        key(&mut st, abi::KEY_HOME, 0);
        assert_eq!(st.scroll(), 0.0);
        st.handle(Event::MouseMove(Mouse { x: 500, y: 400, buttons: 0, mods: 0 }));
        st.handle(Event::Scroll { dx: 0, dy: -3, mods: 0 });
        assert!(st.scroll() > 100.0);
        // Reload keeps the position.
        let s = st.scroll();
        key(&mut st, 'r' as u32, abi::MOD_CTRL);
        assert_eq!(st.scroll(), s);
    });
}

#[test]
fn drawing_doesnt_panic_at_odd_sizes() {
    big(|| {
        let mut st = setup();
        go(&mut st, "test.aether/styled.html");
        for (w, h) in [(1000u32, 700u32), (320, 240), (1600, 1000), (1, 1), (200, 60)] {
            st.resize(w as i32, h as i32);
            let mut px = vec![0u32; (w * h) as usize];
            st.draw(&mut px, w, h, w);
        }
    });
}

#[test]
fn scripts_run_and_respond() {
    big(|| {
        let mut st = setup();
        go(&mut st, "test.aether/scripted.html");
        let t = st.page_text();
        assert!(t.contains("Scripts and files are running"), "{t}\n{:?}", st.script_errors());
        assert!(t.contains("Item three"), "{t}");
        assert!(t.contains("Fetched: hello from the same site"), "{t}");
        assert!(!t.contains("Scripts are off"), "{t}");
        // The other site's script never loaded, and its request was refused.
        assert!(st.window_title().contains("blocked"), "{}", st.window_title());
        // Clicks reach the page's handlers.
        let p = st.find_text("Clicked 0 times").unwrap();
        click(&mut st, p);
        click(&mut st, p);
        assert!(st.page_text().contains("Clicked 2 times"), "{}", st.page_text());
        // Typing fires input events; the page handles its own form.
        key(&mut st, '\t' as u32, 0);
        type_text(&mut st, "cats");
        assert!(st.page_text().contains("Typing: cats"), "{}", st.page_text());
        key(&mut st, '\n' as u32, 0);
        assert!(st.page_text().contains("You searched for cats"), "{}", st.page_text());
        assert_eq!(st.address(), "https://test.aether/scripted.html");
        // A link the page handles changes the address without loading.
        let p = st.find_link("elsewhere").unwrap();
        click(&mut st, p);
        assert!(st.page_text().contains("Handled by the page"), "{}", st.page_text());
        assert_eq!(st.address(), "https://test.aether/handled");
        assert!(st.script_errors().is_empty(), "{:?}", st.script_errors());
    });
}

#[test]
fn tabs_keep_separate_pages_and_history() {
    big(|| {
        let mut st = setup();
        go(&mut st, "test.aether/article.html");
        assert!(st.page_text().contains("Lighthouse"));
        assert_eq!(st.tab_count(), 1);
        // Ctrl+T opens a new tab on about:home, with an empty address bar.
        key(&mut st, 't' as u32, abi::MOD_CTRL);
        assert_eq!(st.tab_count(), 2);
        assert_eq!(st.active_tab(), 1);
        assert_eq!(st.address(), "");
        assert!(st.page_text().contains("quieter window"), "{}", st.page_text());
        go(&mut st, "test.aether/listings.html");
        assert!(st.page_text().contains("Branches"));
        // Ctrl+1 and Ctrl+2 switch without losing either page.
        key(&mut st, '1' as u32, abi::MOD_CTRL);
        assert_eq!(st.active_tab(), 0);
        assert!(st.page_text().contains("Lighthouse"));
        key(&mut st, '2' as u32, abi::MOD_CTRL);
        assert!(st.page_text().contains("Branches"));
        // Ctrl+Tab wraps back to the first tab.
        key(&mut st, '\t' as u32, abi::MOD_CTRL);
        assert_eq!(st.active_tab(), 0);
        // Closing the first tab keeps the window and selects the other one.
        assert!(st.handle(Event::Key { key: 'w' as u32, mods: abi::MOD_CTRL }));
        assert_eq!(st.tab_count(), 1);
        assert!(st.page_text().contains("Branches"));
        // Closing the last tab asks the window to close.
        assert!(!st.handle(Event::Key { key: 'w' as u32, mods: abi::MOD_CTRL }));
    });
}

#[test]
fn tab_strip_clicks_select_and_close() {
    big(|| {
        let mut st = setup();
        key(&mut st, 't' as u32, abi::MOD_CTRL);
        go(&mut st, "test.aether/article.html");
        assert_eq!(st.tab_count(), 2);
        // The first tab starts at x=8; with two tabs it is 220 px wide.
        click(&mut st, (100, 20));
        assert_eq!(st.active_tab(), 0);
        // Its close button is at the right edge.
        click(&mut st, (8 + 220 - 15, 20));
        assert_eq!(st.tab_count(), 1);
        // The + button opens a tab.
        click(&mut st, (8 + 220 + 6 + 14, 20));
        assert_eq!(st.tab_count(), 2);
        // Drawing a window with tabs is safe.
        let (w, h) = (1000u32, 700u32);
        let mut px = vec![0u32; (w * h) as usize];
        st.draw(&mut px, w, h, w);
    });
}

#[test]
fn new_tab_page_is_blank_and_ready() {
    big(|| {
        let mut st = setup();
        key(&mut st, 't' as u32, abi::MOD_CTRL);
        assert_eq!(st.tab_title(1), "New Tab");
        // Typing goes into the address bar of the new tab, not the old page.
        type_text(&mut st, "test.aether/");
        key(&mut st, '\n' as u32, 0);
        assert_eq!(st.address(), "https://test.aether/");
        key(&mut st, '1' as u32, abi::MOD_CTRL);
        assert_eq!(st.address(), "");
    });
}

#[test]
fn pictures_render_in_a_page() {
    big(|| {
        let mut st = setup();
        go(&mut st, "test.aether/images.html");
        assert_eq!(st.image_count(), 7, "{:?}", st.status());
        assert!(st.page_text().contains("Pictures"));
        let (w, h) = (1000u32, 700u32);
        let mut px = vec![0u32; (w * h) as usize];
        st.draw(&mut px, w, h, w);
        // The decoded gradient is drawn in colour, not the grey placeholder.
        assert!(px.iter().any(|&p| (p & 0xFF) > 40 && ((p >> 8) & 0xFF) > 100), "no picture pixels");
    });
}

#[test]
fn scripts_can_be_turned_off_per_site() {
    big(|| {
        let mut st = setup();
        go(&mut st, "test.aether/scripted.html");
        assert!(st.page_text().contains("are running"));
        // The scripts button: own -> all -> off (each click reloads).
        st.toggle_scripts_for_test();
        st.toggle_scripts_for_test();
        let t = st.page_text();
        assert!(t.contains("Waiting for scripts") && t.contains("Scripts are off"), "{t}");
        st.toggle_scripts_for_test();
        assert!(st.page_text().contains("are running"));
    });
}
