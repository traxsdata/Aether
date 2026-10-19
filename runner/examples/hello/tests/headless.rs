//! headless.rs - runs the hello binary on the headless backend with scripted
//! clicks and typing, and checks the PNG frames it writes.
//!
//! The PNGs land in target/hello-frames/ for a person to look at.

use std::path::PathBuf;
use std::process::Command;

fn out_dir() -> PathBuf {
    let dir = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join("hello-frames");
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// Reads width, height and the RGB rows of one of our uncompressed PNGs.
fn read_png(path: &PathBuf) -> (u32, u32, Vec<u8>) {
    let f = std::fs::read(path).unwrap();
    assert_eq!(&f[..8], b"\x89PNG\r\n\x1a\n");
    let w = u32::from_be_bytes(f[16..20].try_into().unwrap());
    let h = u32::from_be_bytes(f[20..24].try_into().unwrap());
    // IDAT follows IHDR (8 + 25 bytes); skip the zlib header, then unpack stored blocks.
    let idat = 8 + 25;
    let len = u32::from_be_bytes(f[idat..idat + 4].try_into().unwrap()) as usize;
    let z = &f[idat + 8..idat + 8 + len];
    let (mut i, mut raw) = (2, Vec::new());
    loop {
        let last = z[i] & 1;
        let n = u16::from_le_bytes([z[i + 1], z[i + 2]]) as usize;
        raw.extend_from_slice(&z[i + 5..i + 5 + n]);
        i += 5 + n;
        if last == 1 {
            break;
        }
    }
    (w, h, raw)
}

fn distinct_colors(raw: &[u8]) -> usize {
    let mut set = std::collections::HashSet::new();
    for px in raw.chunks(3) {
        set.insert([px[0], px[1], px.get(2).copied().unwrap_or(0)]);
    }
    set.len()
}

#[test]
fn clicks_types_and_renders() {
    let dir = out_dir();
    let d = dir.display();
    let script = format!(
        "frame 3\n\
         dump {d}/0-start.png\n\
         click 50 75\nframe 2\nclick 50 75\nframe 2\n\
         dump {d}/1-clicked.png\n\
         click 200 52\nframe\nkey ctrl+a\ntype Rust\nkey enter\nframe 2\n\
         dump {d}/2-typed.png\n\
         down 20 102\nframe\nmove 60 102\nframe\nmove 90 102\nframe\nup 90 102\nframe 3\n\
         dump {d}/3-slider.png\n"
    );
    let script_path = dir.join("script.txt");
    std::fs::write(&script_path, script).unwrap();
    let out = Command::new(env!("CARGO_BIN_EXE_hello"))
        .env_remove("AETHER")
        .env("AETHER_SCRIPT", &script_path)
        .env("AETHER_STORAGE_DIR", dir.join("storage"))
        .env("AETHER_STATS", "1")
        .output()
        .unwrap();
    let log = String::from_utf8_lossy(&out.stderr);
    eprintln!("{log}");
    assert!(out.status.success(), "hello failed");

    for name in ["0-start", "1-clicked", "2-typed", "3-slider"] {
        let (w, h, raw) = read_png(&dir.join(format!("{name}.png")));
        assert_eq!((w, h), (640, 480));
        assert_eq!(raw.len(), (h * (1 + 3 * w)) as usize);
        // Text, widgets and the anti-aliased plot make many colours.
        assert!(distinct_colors(&raw) > 50, "{name} looks blank");
    }
    // Clicks and typing were saved through eframe::Storage on exit.
    let saved = std::fs::read_to_string(dir.join("storage/app.ron")).unwrap();
    assert!(saved.contains("Rust"), "{saved}");
    assert!(saved.contains(",2)"), "{saved}");
    // Dragging the slider moved it away from 2.0 (its value is in the label).
    let (_, _, before) = read_png(&dir.join("2-typed.png"));
    let (_, _, after) = read_png(&dir.join("3-slider.png"));
    assert_ne!(before, after, "the slider drag changed nothing");
}
