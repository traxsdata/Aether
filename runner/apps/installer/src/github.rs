//! GitHub releases for the App Store.
//!
//! The installer talks to GitHub's public API directly. Craft apps are built
//! from their github.com/storytold sources by the workflow in
//! github.com/traxsdata/Aether-CraftApps, which publishes every package to one
//! release (`craft-apps-latest`) as `gridcraft-0.8.0-aether-x86_64.aeapp`
//! (GitHub won't take a `.app` asset). One API call lists every app; the
//! version comes from the asset name. A `.app` or `.app.zip` asset is still
//! accepted for older releases. The kernel verifies each package's header,
//! SHA-256 and ELF image before installing it. OS releases may publish an
//! `.iso`.

use std::time::Duration;

use webnet::crypto::Sha256;
use webnet::{fetch_with, Config, Request, Url};

use crate::json::{self, Json};

pub const CRAFT_OWNER: &str = "traxsdata";
pub const CRAFT_REPO: &str = "Aether-CraftApps";
pub const CRAFT_TAG: &str = "craft-apps-latest";
pub const OS_OWNER: &str = "traxsdata";
pub const OS_REPO: &str = "Aether";

#[derive(Clone, Debug)]
pub struct Asset {
    pub name: String,
    pub url: String,
    pub size: u64,
    pub digest: Option<String>,
}

#[derive(Clone, Debug)]
pub struct Release {
    pub tag: String,
    pub assets: Vec<Asset>,
}

impl Release {
    pub fn version(&self) -> &str { self.tag.strip_prefix('v').unwrap_or(&self.tag) }
}

pub struct CatalogApp {
    pub repo: &'static str,
    pub title: &'static str,
    pub color: u32,
}

pub const CATALOG: &[CatalogApp] = &[
    CatalogApp { repo: "artcraft", title: "ArtCraft", color: 0xF97316 },
    CatalogApp { repo: "vectorcraft", title: "VectorCraft", color: 0xF97316 },
    CatalogApp { repo: "photocraft", title: "PhotoCraft", color: 0x3B82F6 },
    CatalogApp { repo: "lightcraft", title: "LightCraft", color: 0xEAB308 },
    CatalogApp { repo: "filmcraft", title: "FilmCraft", color: 0xE11D48 },
    CatalogApp { repo: "pdfcraft", title: "PdfCraft", color: 0xDC2626 },
    CatalogApp { repo: "effectcraft", title: "EffectCraft", color: 0x8B5CF6 },
    CatalogApp { repo: "designcraft", title: "DesignCraft", color: 0xEC4899 },
    CatalogApp { repo: "figcraft", title: "FigCraft", color: 0xA855F7 },
    CatalogApp { repo: "wordcraft", title: "WordCraft", color: 0x2563EB },
    CatalogApp { repo: "soundcraft", title: "SoundCraft", color: 0x14B8A6 },
    CatalogApp { repo: "deckcraft", title: "DeckCraft", color: 0xF59E0B },
    CatalogApp { repo: "cadcraft", title: "CADCraft", color: 0x64748B },
    CatalogApp { repo: "gridcraft", title: "GridCraft", color: 0x22A06B },
    CatalogApp { repo: "craft-launcher", title: "ArtCraft Launcher", color: 0x38E1F5 },
];

fn config() -> Config {
    Config {
        connect_timeout: Duration::from_secs(30),
        read_timeout: Duration::from_secs(90),
        total_timeout: Duration::from_secs(300),
        ..Config::default()
    }
}

/// The shared release that carries every Craft app package.
pub fn fetch_craft_release() -> Result<Release, String> {
    fetch_release_at(&format!("https://api.github.com/repos/{CRAFT_OWNER}/{CRAFT_REPO}/releases/tags/{CRAFT_TAG}"))
}

pub fn fetch_os_release() -> Result<Release, String> {
    fetch_release_at(&format!("https://api.github.com/repos/{OS_OWNER}/{OS_REPO}/releases/latest"))
}

/// One app's view of the shared release: the packages named
/// `<repo>-<version>-...`, tagged with that version. None when the release
/// has no package for the app.
pub fn app_release(shared: &Release, repo: &str) -> Option<Release> {
    let prefix = format!("{repo}-");
    let mut best: Option<(String, Vec<Asset>)> = None;
    for a in &shared.assets {
        let Some(rest) = a.name.strip_prefix(&prefix) else { continue };
        if !rest.starts_with(|c: char| c.is_ascii_digit()) || !is_package_name(&a.name) {
            continue;
        }
        let version = rest.split("-aether").next().unwrap_or(rest).to_string();
        match &mut best {
            Some((v, list)) if *v == version => list.push(a.clone()),
            Some((v, _)) if !is_newer(&version, v) => {}
            _ => best = Some((version, vec![a.clone()])),
        }
    }
    best.map(|(tag, assets)| Release { tag, assets })
}

fn is_package_name(name: &str) -> bool {
    let n = name.to_ascii_lowercase();
    n.ends_with(".aeapp") || n.ends_with(".app") || n.ends_with(".app.zip")
}

fn fetch_release_at(url: &str) -> Result<Release, String> {
    let url = Url::parse(url).map_err(|e| format!("bad GitHub URL: {e}"))?;
    let mut req = Request::get(url);
    req.max_bytes = 2 * 1024 * 1024;
    let resp = fetch_with(&config(), &req).map_err(|e| e.to_string())?;
    if resp.status == 404 {
        return Err("No release has been published yet.".into());
    }
    if resp.status == 403 {
        return Err("GitHub's rate limit was reached. Try again later.".into());
    }
    if resp.status != 200 {
        return Err(format!("GitHub answered HTTP {}.", resp.status));
    }
    let text = String::from_utf8_lossy(&resp.body);
    parse_release(&text)
}

fn parse_release(text: &str) -> Result<Release, String> {
    let j = json::parse(text).map_err(|e| format!("GitHub's answer wasn't valid JSON: {e}"))?;
    let tag = j.get("tag_name").and_then(Json::as_str).ok_or("the release has no tag")?.to_string();
    let mut assets = Vec::new();
    for a in j.get("assets").and_then(Json::as_array).unwrap_or(&[]) {
        let Some(name) = a.get("name").and_then(Json::as_str) else { continue };
        let Some(url) = a.get("browser_download_url").and_then(Json::as_str) else { continue };
        assets.push(Asset {
            name: name.to_string(),
            url: url.to_string(),
            size: a.get("size").and_then(Json::as_u64).unwrap_or(0),
            digest: a.get("digest").and_then(Json::as_str).map(str::to_string),
        });
    }
    Ok(Release { tag, assets })
}

/// The best æther package in a release, if it published one.
pub fn app_asset(release: &Release) -> Option<&Asset> {
    release
        .assets
        .iter()
        .filter(|a| is_package_name(&a.name))
        .max_by_key(|a| {
            let n = a.name.to_ascii_lowercase();
            let mut score = 0;
            if a.digest.is_some() {
                score += 16; // prefer a release that publishes its digest
            }
            if n.ends_with(".aeapp") || n.ends_with(".app") {
                score += 8; // a direct package is simpler than a ZIP
            }
            if n.contains("aether") {
                score += 4;
            }
            if n.contains("x86_64") || n.contains("amd64") {
                score += 2;
            }
            if n.contains("musl") {
                score += 1;
            }
            score
        })
}

/// The package bytes inside a release asset. A `.app.zip` is unpacked with
/// the small reader in `zip.rs` and its entry CRC checked; a direct `.app`
/// is returned as it is. The release digest was checked by the caller.
pub fn package_bytes(asset: &Asset, bytes: Vec<u8>) -> Result<Vec<u8>, String> {
    if asset.name.to_ascii_lowercase().ends_with(".zip") {
        crate::zip::extract_app(&bytes)
    } else {
        Ok(bytes)
    }
}

/// The installed-system payload published by a release.
pub fn system_asset(release: &Release) -> Option<&Asset> {
    release.assets.iter().find(|a| {
        let n = a.name.to_ascii_lowercase();
        (n.ends_with(".img") || n.ends_with(".sysimage")) && n.contains("system") && n.contains("x86_64")
    })
}

/// The small fixed FAT ESP image (loader only) published by a release.
pub fn boot_asset(release: &Release) -> Option<&Asset> {
    release.assets.iter().find(|a| {
        let n = a.name.to_ascii_lowercase();
        n.ends_with(".img") && n.contains("boot") && n.contains("x86_64")
    })
}

pub fn iso_asset(release: &Release) -> Option<&Asset> {
    release.assets.iter().find(|a| {
        let n = a.name.to_ascii_lowercase();
        n.ends_with(".iso") && (n.contains("aether") || n.contains("os"))
    })
}

pub fn fetch_bytes(url: &str, max: usize) -> Result<Vec<u8>, String> {
    let url = Url::parse(url).map_err(|e| format!("bad URL: {e}"))?;
    let mut req = Request::get(url);
    req.max_bytes = max;
    let resp = fetch_with(&config(), &req).map_err(|e| e.to_string())?;
    if resp.status != 200 {
        return Err(format!("The download answered HTTP {}.", resp.status));
    }
    Ok(resp.body)
}

pub fn verify_digest(bytes: &[u8], digest: Option<&str>) -> Result<(), String> {
    let Some(d) = digest else {
        return Err("The release doesn't publish a SHA-256 digest; refusing to install it.".into());
    };
    let Some(hex) = d.strip_prefix("sha256:") else {
        return Err("The release's checksum isn't a sha256: digest; refusing to install it.".into());
    };
    if hex.len() != 64 {
        return Err("The release's checksum isn't a SHA-256 digest.".into());
    }
    let mut h = Sha256::new();
    h.update(bytes);
    let got = h.finish();
    let mut want = Vec::new();
    let b = hex.as_bytes();
    for i in (0..64).step_by(2) {
        let hi = (b[i] as char).to_digit(16).ok_or("bad checksum")?;
        let lo = (b[i + 1] as char).to_digit(16).ok_or("bad checksum")?;
        want.push((hi * 16 + lo) as u8);
    }
    if got.as_slice() == want.as_slice() {
        Ok(())
    } else {
        Err("The download doesn't match the release's SHA-256 checksum.".into())
    }
}

/// True when `latest` is a newer dotted version than `installed`.
pub fn is_newer(latest: &str, installed: &str) -> bool {
    let a = numbers(latest);
    let b = numbers(installed);
    for i in 0..a.len().max(b.len()) {
        let x = a.get(i).copied().unwrap_or(0);
        let y = b.get(i).copied().unwrap_or(0);
        if x != y {
            return x > y;
        }
    }
    false
}

fn numbers(v: &str) -> Vec<u64> {
    let v = v.trim().trim_start_matches('v');
    v.split(|c: char| !c.is_ascii_digit()).filter(|s| !s.is_empty()).filter_map(|s| s.parse().ok()).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn version_order() {
        assert!(is_newer("v0.8.0", "0.7.0"));
        assert!(is_newer("1.0.0", "0.99.9"));
        assert!(!is_newer("0.8.0", "0.8.0"));
        assert!(!is_newer("0.7.9", "0.8.0"));
    }

    #[test]
    fn digest_is_required_and_checked() {
        assert!(verify_digest(b"", None).is_err());
        assert!(verify_digest(b"", Some("md5:00")).is_err());
        assert!(verify_digest(
            b"",
            Some("sha256:e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855")
        )
        .is_ok());
        assert!(verify_digest(b"x", Some("sha256:e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"))
            .is_err());
    }

    #[test]
    fn picks_an_aether_app() {
        let r = Release {
            tag: "v1.0.0".into(),
            assets: vec![
                Asset { name: "app-linux-x86_64.tar.gz".into(), url: "x".into(), size: 1, digest: None },
                Asset { name: "app-1.0.0-aether-x86_64.app".into(), url: "y".into(), size: 2, digest: None },
            ],
        };
        assert_eq!(app_asset(&r).unwrap().url, "y");
    }

    #[test]
    fn picks_a_zipped_aether_app() {
        let r = Release {
            tag: "v1.0.0".into(),
            assets: vec![
                Asset { name: "app-linux-x86_64.tar.gz".into(), url: "x".into(), size: 1, digest: None },
                Asset { name: "app-1.0.0-aether-x86_64.app.zip".into(), url: "z".into(), size: 2, digest: None },
            ],
        };
        assert_eq!(app_asset(&r).unwrap().url, "z");
    }

    fn asset(name: &str) -> Asset { Asset { name: name.into(), url: name.into(), size: 1, digest: None } }

    #[test]
    fn splits_the_shared_release_by_app() {
        let shared = Release {
            tag: CRAFT_TAG.into(),
            assets: vec![
                asset("craft-apps.json"),
                asset("gridcraft.json"),
                asset("gridcraft-0.8.0-aether-x86_64.app.zip"),
                asset("gridcraft-0.8.0-aether-x86_64.aeapp"),
                asset("gridcraft-0.7.0-aether-x86_64.aeapp"),
                asset("gridcraft-pro-1.0.0-aether-x86_64.aeapp"),
                asset("photocraft-0.10.0-aether-x86_64.aeapp"),
            ],
        };
        let g = app_release(&shared, "gridcraft").unwrap();
        assert_eq!(g.version(), "0.8.0");
        assert_eq!(app_asset(&g).unwrap().name, "gridcraft-0.8.0-aether-x86_64.aeapp");
        assert_eq!(app_release(&shared, "photocraft").unwrap().version(), "0.10.0");
        assert!(app_release(&shared, "vectorcraft").is_none());
        assert!(app_release(&shared, "craft-launcher").is_none());
    }
}
