//! Image decoding: the formats a page actually uses, and the limits that
//! keep a hostile picture from becoming a memory problem.

use browser::image::{decode, ImageError};
use browser::layout::Item;
use browser::page;
use browser::net::{Request, Url};
use browser::script::Policy;
use std::sync::atomic::AtomicBool;

fn fixture(name: &str) -> Vec<u8> {
    std::fs::read(format!("{}/testpages/images/{name}", env!("CARGO_MANIFEST_DIR"))).unwrap()
}

fn px(img: &browser::image::Image, x: u32, y: u32) -> [u8; 4] {
    let o = ((y * img.width + x) * 4) as usize;
    [img.pixels[o], img.pixels[o + 1], img.pixels[o + 2], img.pixels[o + 3]]
}

#[test]
fn png_rgba_and_palette() {
    let img = decode(&fixture("quad.png"), Some("image/png")).unwrap();
    assert_eq!((img.width, img.height), (4, 4));
    assert_eq!(px(&img, 0, 0), [255, 0, 0, 255]);
    assert_eq!(px(&img, 3, 0), [0, 255, 0, 255]);
    assert_eq!(px(&img, 0, 3), [0, 0, 255, 255]);
    assert_eq!(px(&img, 3, 3), [255, 255, 0, 128]);

    let img = decode(&fixture("palette.png"), Some("image/png")).unwrap();
    assert_eq!((img.width, img.height), (4, 4));
    assert_eq!(px(&img, 0, 0)[3], 0, "index 0 is transparent");
    assert_eq!(px(&img, 1, 0), [255, 0, 0, 255]);
    assert_eq!(px(&img, 2, 0), [0, 255, 0, 255]);
    assert_eq!(px(&img, 3, 0), [0, 0, 255, 255]);
}

#[test]
fn gif_and_bmp() {
    let img = decode(&fixture("quad.gif"), Some("image/gif")).unwrap();
    assert_eq!((img.width, img.height), (4, 4));
    assert_eq!(px(&img, 0, 0)[3], 0);
    assert_eq!(px(&img, 1, 0), [255, 0, 0, 255]);
    assert_eq!(px(&img, 2, 0), [0, 255, 0, 255]);

    let img = decode(&fixture("quad.bmp"), Some("image/bmp")).unwrap();
    assert_eq!((img.width, img.height), (4, 4));
    assert_eq!(px(&img, 0, 0), [255, 0, 0, 255]);
    assert_eq!(px(&img, 3, 0), [0, 255, 0, 255]);
    assert_eq!(px(&img, 0, 3), [0, 0, 255, 255]);
}

#[test]
fn jpeg_baseline_and_progressive() {
    for name in [
        "gradient-420.jpg",
        "gradient-444.jpg",
        "gradient-420-progressive.jpg",
        "gradient-444-progressive.jpg",
    ] {
        let img = decode(&fixture(name), Some("image/jpeg")).unwrap();
        assert_eq!((img.width, img.height), (16, 16), "{name}");
        // The top-left is near (0, 0, 128), the bottom-right near (240, 240, 128).
        let a = px(&img, 0, 0);
        let b = px(&img, 15, 15);
        assert!(a[0] < 40 && a[1] < 40, "{name}: {a:?}");
        assert!(b[0] > 180 && b[1] > 180, "{name}: {b:?}");
        assert!(a[2].abs_diff(128) < 40 && b[2].abs_diff(128) < 40, "{name}: blue");
    }
    for name in ["gray.jpg", "gray-progressive.jpg"] {
        let img = decode(&fixture(name), Some("image/jpeg")).unwrap();
        assert_eq!((img.width, img.height), (16, 16));
        let a = px(&img, 0, 0);
        assert_eq!(a[0], a[1], "{name}");
        assert_eq!(a[1], a[2], "{name}");
    }
}

#[test]
fn unknown_and_damaged_images_are_errors() {
    assert!(matches!(decode(b"not an image", None), Err(ImageError::Unsupported(_))));
    assert!(matches!(decode(b"\x89PNG\r\n\x1a\n", None), Err(ImageError::Corrupt(_))));
    let mut png = fixture("quad.png");
    png[40] ^= 0xFF;
    assert!(decode(&png, Some("image/png")).is_err());
}

#[test]
fn huge_dimensions_are_refused_before_allocating() {
    // A PNG header with valid CRCs that claims 100000 x 100000 pixels.
    let png = fixture("huge.png");
    assert!(matches!(decode(&png, Some("image/png")), Err(ImageError::TooLarge)));
}

fn big<F: FnOnce() + Send + 'static>(f: F) {
    std::thread::Builder::new().stack_size(32 << 20).spawn(f).unwrap().join().unwrap();
}

fn load(path: &str) -> page::Loaded {
    browser::testserve::set_root(format!("{}/testpages", env!("CARGO_MANIFEST_DIR")).into());
    let url = Url::parse(&format!("https://test.aether/{path}")).unwrap();
    page::load(Request::get(url), None, &browser::testserve::fetch, &AtomicBool::new(false), &Policy::default()).unwrap()
}

#[test]
fn page_fetches_pictures_and_uses_their_size() {
    big(|| {
        let l = load("images.html");
        assert_eq!(l.images.len(), 7, "{}", l.blocked.join("\n"));
        assert!(l.blocked.iter().any(|b| b.contains("missing.png")), "{:?}", l.blocked);
        let pg = page::lay_out(&l, 800.0, 600.0);
        let quad = pg.items.iter().find_map(|i| match i {
            Item::Image { w, h, alt, .. } if alt == "Four colours" => Some((*w, *h)),
            _ => None,
        });
        assert_eq!(quad, Some((4.0, 4.0)));
        // An explicit width/height from the page wins over the picture's size.
        let grad = pg.items.iter().find_map(|i| match i {
            Item::Image { w, h, alt, .. } if alt == "Gradient" => Some((*w, *h)),
            _ => None,
        });
        assert_eq!(grad, Some((64.0, 64.0)));
        // A picture inside a table cell gets its intrinsic size too.
        let table = pg.items.iter().find_map(|i| match i {
            Item::Image { w, h, alt, .. } if alt == "Table picture" => Some((*w, *h)),
            _ => None,
        });
        assert_eq!(table, Some((4.0, 4.0)));
        // max-width constrains a picture and keeps its aspect ratio.
        let narrow = pg.items.iter().find_map(|i| match i {
            Item::Image { w, h, alt, .. } if alt == "Narrow lake" => Some((*w, *h)),
            _ => None,
        });
        assert_eq!(narrow, Some((64.0, 40.0)));
        let mut px = vec![0u32; 800 * 600];
        let mut cache = browser::font::Cache::default();
        let form = page::initial_form(&l.doc);
        let mut cv = browser::paint::Canvas::new(&mut px, 800, 600, 800);
        browser::paint::paint_page(&mut cv, &mut cache, &pg, &l.images, 0, 0, 0.0, &form);
    });
}

#[test]
fn mixed_content_pictures_are_not_fetched() {
    big(|| {
        let l = load("mixed.html");
        assert!(l.images.is_empty(), "an https page loaded an http picture");
        assert!(l.blocked.iter().any(|b| b.contains("plain-http")), "{:?}", l.blocked);
    });
}

#[test]
fn image_decoders_survive_damaged_input() {
    let mut seed = 0x1234_5678_9abc_def0u64;
    let mut next = || {
        seed ^= seed << 13;
        seed ^= seed >> 7;
        seed ^= seed << 17;
        seed
    };
    for _ in 0..2000 {
        let n = (next() % 300) as usize;
        let data: Vec<u8> = (0..n).map(|_| next() as u8).collect();
        let _ = decode(&data, None);
        let _ = decode(&data, Some("image/png"));
        let _ = decode(&data, Some("image/jpeg"));
    }
    for name in ["quad.png", "gradient-420.jpg", "gradient-420-progressive.jpg", "quad.gif", "quad.bmp"] {
        let base = fixture(name);
        for _ in 0..200 {
            let mut data = base.clone();
            if data.is_empty() {
                continue;
            }
            let at = (next() as usize) % data.len();
            data[at] ^= (next() as u8) | 1;
            let _ = decode(&data, None);
        }
    }
}

