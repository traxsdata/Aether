//! App Store / netboot installer for æther 0.8: install and update Craft
//! apps, install a fresh system, and write OS updates to the inactive A/B
//! slot, directly from GitHub.
//!
//! A clean-room Rust system app: the UI is drawn with the kernel apps' own
//! software graphics and Facet font through `system-ui`; only `std`,
//! `aether-host`, the in-tree `webnet` and the in-tree GitHub/JSON code are
//! used. No egui, no third-party crates, no bundled fonts.

mod github;
mod json;
mod zip;

use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::Arc;
use std::time::{Duration, Instant};

use aether_host::abi::*;
use aether_host::{sys, Backend, Event, PickMode};
use system_ui::{ellipsize, font_h, text_width, App, Canvas, Font, Rect};

use github::{CatalogApp, Release};

const W: u32 = 900;
const H: u32 = 610;

const C_BG: u32 = 0x0D1222;
const C_BAR: u32 = 0x131A31;
const C_BTN: u32 = 0x1D2647;
const C_BTN_HOT: u32 = 0x2A3560;
const C_TEXT: u32 = 0xE2E8FA;
const C_DIM: u32 = 0x7C87A8;
const C_ACCENT: u32 = 0x38E1F5;
const C_WARN: u32 = 0xFFB547;
const C_ERR: u32 = 0xFF5C7A;

/// An installed app's icon, copied out of its package with SYS_APP_ICON.
struct AppIcon {
    width: u32,
    height: u32,
    rgba: Vec<u8>,
}

#[derive(Clone)]
struct AppRow {
    repo: String,
    title: String,
    color: u32,
    installed: Option<app_info>,
    latest: Option<Release>,
    status: String,
    busy: bool,
    icon: Option<Arc<AppIcon>>,
    icon_name: String,
    icon_version: String,
}

impl AppRow {
    fn new(a: &CatalogApp) -> Self {
        Self {
            repo: a.repo.into(),
            title: a.title.into(),
            color: a.color,
            installed: None,
            latest: None,
            status: String::new(),
            busy: false,
            icon: None,
            icon_name: String::new(),
            icon_version: String::new(),
        }
    }

    fn installed_name(&self) -> Option<String> {
        self.installed.map(|i| aether_host::abi::cstr(&i.name).to_string())
    }

    fn installed_version(&self) -> String {
        self.installed.map(|i| aether_host::abi::cstr(&i.version).to_string()).unwrap_or_default()
    }

    fn latest_version(&self) -> Option<String> {
        self.latest.as_ref().map(|r| r.version().to_string())
    }

    fn update_available(&self) -> bool {
        match (&self.latest, self.installed) {
            (Some(r), Some(i)) => github::is_newer(r.version(), aether_host::abi::cstr(&i.version)),
            _ => false,
        }
    }

    fn pending(&self) -> bool { self.installed.is_some_and(|i| i.update_pending != 0) }
}

enum Msg {
    Checked(String, Result<Release, String>),
    AllChecked,
    Installed(String, Result<(), String>),
    AllInstalled,
    OsChecked(Result<Release, String>),
    InstallFinished(Result<(), String>),
    UpdateFinished(Result<(), String>),
}

struct PendingReplace {
    name: String,
    new_source: String,
    old_source: String,
    data: Vec<u8>,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Tab {
    Apps,
    Updates,
    Os,
}

struct Installer {
    tab: Tab,
    apps: Vec<AppRow>,
    tx: Sender<Msg>,
    rx: Receiver<Msg>,
    status: String,
    checking: bool,
    installing: bool,
    os: os_info,
    os_latest: Option<Release>,
    os_status: String,
    os_busy: bool,
    netboot: bool,
    disks: Vec<disk_info>,
    disk_sel: usize,
    passphrase: String,
    passphrase2: String,
    pass_focus: bool,
    pass_focus2: bool,
    install_busy: bool,
    install_status: String,
    install_ok: bool,
    update: update_info,
    update_busy: bool,
    update_status: String,
    update_done: bool,
    pending_replace: Option<PendingReplace>,
    mouse: (i32, i32),
    scroll: usize,
    last_refresh: Instant,
}

impl Installer {
    fn new() -> Self {
        let (tx, rx) = mpsc::channel();
        let os = sys::os_info().unwrap_or_default();
        let update = sys::update_info().unwrap_or_default();
        let netboot = update.boot_flags & BOOT_FLAG_NETBOOT != 0;
        let mut s = Self {
            tab: if netboot { Tab::Os } else { Tab::Apps },
            apps: github::CATALOG.iter().map(AppRow::new).collect(),
            tx,
            rx,
            status: "Choose an app to install, or check GitHub for updates.".into(),
            checking: false,
            installing: false,
            os,
            os_latest: None,
            os_status: String::new(),
            os_busy: false,
            netboot,
            disks: Vec::new(),
            disk_sel: 0,
            passphrase: String::new(),
            passphrase2: String::new(),
            pass_focus: false,
            pass_focus2: false,
            install_busy: false,
            install_status: String::new(),
            install_ok: false,
            update,
            update_busy: false,
            update_status: String::new(),
            update_done: false,
            pending_replace: None,
            mouse: (0, 0),
            scroll: 0,
            last_refresh: Instant::now() - Duration::from_secs(10),
        };
        s.refresh_apps();
        if netboot {
            s.refresh_disks();
        }
        s
    }

    fn refresh_disks(&mut self) {
        let mut list = [disk_info::default(); 8];
        match sys::disk_list(&mut list) {
            Ok(n) => self.disks = list[..n].to_vec(),
            Err(e) => self.install_status = format!("Couldn't list disks: {e}"),
        }
    }

    fn refresh_apps(&mut self) {
        let mut list = [app_info::default(); 32];
        let n = sys::app_list(&mut list).unwrap_or(0);
        for row in &mut self.apps {
            row.installed = list[..n].iter().find(|a| installed_matches(a, &row.repo, &row.title)).copied();
            let name = row.installed_name().unwrap_or_default();
            let version = row.installed_version();
            if name.is_empty() {
                row.icon = None;
                row.icon_name.clear();
                row.icon_version.clear();
                continue;
            }
            if row.icon.is_none() || row.icon_name != name || row.icon_version != version {
                let mut pixels = vec![0u8; APP_ICON_MAX * APP_ICON_MAX * 4];
                row.icon = match sys::app_icon(&name, &mut pixels) {
                    Ok(Some((w, h))) => {
                        pixels.truncate(w as usize * h as usize * 4);
                        Some(Arc::new(AppIcon { width: w, height: h, rgba: pixels }))
                    }
                    _ => None,
                };
                row.icon_name = name;
                row.icon_version = version;
            }
        }
        self.last_refresh = Instant::now();
    }

    fn poll(&mut self) -> bool {
        let mut changed = false;
        while let Ok(msg) = self.rx.try_recv() {
            changed = true;
            match msg {
                Msg::Checked(repo, result) => {
                    if let Some(row) = self.apps.iter_mut().find(|r| r.repo == repo) {
                        row.busy = false;
                        match result {
                            Ok(r) => {
                                row.status = match github::app_asset(&r) {
                                    Some(a) if a.digest.is_some() => "Ready to install".into(),
                                    Some(_) => "Release has no SHA-256 digest".into(),
                                    None => "No æther build in this release yet".into(),
                                };
                                row.latest = Some(r);
                            }
                            Err(e) => {
                                row.status = e;
                                row.latest = None;
                            }
                        }
                    }
                }
                Msg::AllChecked => {
                    self.checking = false;
                    self.refresh_apps();
                    self.status = "Checked GitHub for every app.".into();
                }
                Msg::Installed(repo, result) => {
                    if let Some(row) = self.apps.iter_mut().find(|r| r.repo == repo) {
                        row.busy = false;
                        match result {
                            Ok(()) => {
                                row.status = "Installed".into();
                                self.status = format!("{} was installed.", row.title);
                            }
                            Err(e) => {
                                row.status = e.clone();
                                self.status = format!("{}: {}", row.title, e);
                            }
                        }
                    }
                    self.refresh_apps();
                }
                Msg::AllInstalled => {
                    self.installing = false;
                    self.refresh_apps();
                    self.status = "Updates installed. Running apps keep their old code until restarted.".into();
                }
                Msg::OsChecked(result) => {
                    self.os_busy = false;
                    match result {
                        Ok(r) => {
                            self.os_status = match github::iso_asset(&r) {
                                Some(a) => format!("{} is available ({}).", r.tag, format_size(a.size)),
                                None => format!("{} is published, but it has no ISO asset.", r.tag),
                            };
                            self.os_latest = Some(r);
                        }
                        Err(e) => {
                            self.os_status = e;
                            self.os_latest = None;
                        }
                    }
                }
                Msg::InstallFinished(result) => {
                    self.install_busy = false;
                    match result {
                        Ok(()) => {
                            self.install_ok = true;
                            self.install_status =
                                "Installed. Remove the netboot medium, then reboot into æther.".into();
                        }
                        Err(e) => self.install_status = e,
                    }
                }
                Msg::UpdateFinished(result) => {
                    self.update_busy = false;
                    match result {
                        Ok(()) => {
                            self.update_done = true;
                            self.update_status =
                                "The update is in the inactive slot. Reboot to try it; it rolls back if it can't start."
                                    .into();
                            self.update = sys::update_info().unwrap_or_default();
                        }
                        Err(e) => self.update_status = e,
                    }
                }
            }
        }
        changed
    }

    fn check_all(&mut self) {
        if self.checking {
            return;
        }
        self.checking = true;
        for row in &mut self.apps {
            row.busy = true;
            row.status = "Checking...".into();
        }
        let tx = self.tx.clone();
        std::thread::spawn(move || {
            let shared = github::fetch_craft_release();
            for app in github::CATALOG {
                let _ = tx.send(Msg::Checked(app.repo.into(), craft_release(&shared, app.repo)));
            }
            let _ = tx.send(Msg::AllChecked);
        });
    }

    fn check_one(&mut self, repo: String) {
        if let Some(row) = self.apps.iter_mut().find(|r| r.repo == repo) {
            if row.busy {
                return;
            }
            row.busy = true;
            row.status = "Checking...".into();
        }
        let tx = self.tx.clone();
        std::thread::spawn(move || {
            let r = craft_release(&github::fetch_craft_release(), &repo);
            let _ = tx.send(Msg::Checked(repo, r));
        });
    }

    fn install(&mut self, repo: String) {
        let Some(row) = self.apps.iter_mut().find(|r| r.repo == repo) else { return };
        let Some(release) = row.latest.clone() else { return };
        let Some(asset) = github::app_asset(&release).cloned() else {
            row.status = "No æther build in this release yet".into();
            return;
        };
        if asset.digest.is_none() {
            row.status = "Release has no SHA-256 digest".into();
            return;
        }
        row.busy = true;
        row.status = format!("Downloading {}...", asset.name);
        let replace = row.installed.is_some();
        let expected = row.installed_name().unwrap_or_else(|| repo.clone());
        let tx = self.tx.clone();
        let repo2 = repo.clone();
        std::thread::spawn(move || {
            let result = (|| {
                let bytes = github::fetch_bytes(&asset.url, APP_MAX_BYTES)?;
                github::verify_digest(&bytes, asset.digest.as_deref())?;
                let bytes = github::package_bytes(&asset, bytes)?;
                check_package_name(&bytes, &expected)?;
                sys::app_install(&bytes, replace).map_err(|e| e.to_string())
            })();
            let _ = tx.send(Msg::Installed(repo2, result));
        });
    }

    fn update_all(&mut self) {
        if self.installing {
            return;
        }
        let jobs: Vec<_> = self
            .apps
            .iter()
            .filter(|r| r.update_available())
            .filter_map(|r| {
                let a = github::app_asset(r.latest.as_ref()?).cloned()?;
                let expected = r.installed_name().unwrap_or_else(|| r.repo.clone());
                Some((r.repo.clone(), a, r.installed.is_some(), expected))
            })
            .collect();
        if jobs.is_empty() {
            self.status = "Everything is up to date.".into();
            return;
        }
        self.installing = true;
        self.status = format!("Installing {} update(s)...", jobs.len());
        let tx = self.tx.clone();
        std::thread::spawn(move || {
            for (repo, asset, replace, expected) in jobs {
                let result = (|| {
                    let bytes = github::fetch_bytes(&asset.url, APP_MAX_BYTES)?;
                    github::verify_digest(&bytes, asset.digest.as_deref())?;
                    let bytes = github::package_bytes(&asset, bytes)?;
                    check_package_name(&bytes, &expected)?;
                    sys::app_install(&bytes, replace).map_err(|e| e.to_string())
                })();
                let _ = tx.send(Msg::Installed(repo, result));
            }
            let _ = tx.send(Msg::AllInstalled);
        });
    }

    fn install_file(&mut self, backend: &mut dyn Backend) {
        let picked = backend.pick_file(PickMode::Open, "");
        let Ok(p) = picked else {
            self.status = "Install from file cancelled.".into();
            return;
        };
        let mut data = Vec::new();
        let mut buf = vec![0u8; 64 * 1024];
        let ok = loop {
            match backend.read(p.handle, &mut buf) {
                Ok(0) => break true,
                Ok(n) => {
                    data.extend_from_slice(&buf[..n]);
                    if data.len() > APP_MAX_BYTES {
                        break false;
                    }
                }
                Err(_) => break false,
            }
        };
        let _ = backend.close(p.handle);
        if !ok {
            self.status = format!("Couldn't read the package (or it is larger than {} MB).", APP_MAX_BYTES >> 20);
            return;
        }
        self.install_bytes(data);
    }

    fn install_bytes(&mut self, data: Vec<u8>) {
        let data = if zip::looks_like_zip(&data) {
            match zip::extract_app(&data) {
                Ok(package) => package,
                Err(e) => {
                    self.status = format!("That ZIP isn't an app package: {e}");
                    return;
                }
            }
        } else {
            data
        };
        let Some((name, _version, source)) = package_meta(&data) else {
            self.status = "That file isn't an æther app package.".into();
            return;
        };
        match sys::app_install(&data, false) {
            Ok(()) => {
                self.status = format!("{name} was installed.");
                self.refresh_apps();
            }
            Err(e) if e.0 == E_EXIST => {
                let old_source = sys::app_info(&name)
                    .map(|a| aether_host::abi::cstr(&a.source).to_string())
                    .unwrap_or_else(|_| "unknown source".into());
                self.pending_replace = Some(PendingReplace { name, new_source: source, old_source, data });
            }
            Err(e) => self.status = format!("Install failed: {e}"),
        }
    }

    fn open(&mut self, row: &AppRow) {
        let Some(name) = row.installed_name() else { return };
        let caps = CAP_WINDOW | CAP_PICK | CAP_STORAGE | CAP_NET;
        match sys::spawn(&name, caps, 0) {
            Ok(_) => self.status = format!("Started {}.", row.title),
            Err(e) => self.status = format!("Couldn't start {}: {e}", row.title),
        }
    }

    fn remove(&mut self, row: &AppRow) {
        let Some(name) = row.installed_name() else { return };
        match sys::app_remove(&name) {
            Ok(()) => {
                self.status = format!("{} was removed.", row.title);
                self.refresh_apps();
            }
            Err(e) => self.status = format!("Couldn't remove {}: {e}", row.title),
        }
    }

    fn check_os(&mut self) {
        if self.os_busy {
            return;
        }
        self.os_busy = true;
        self.os_status = "Checking GitHub...".into();
        let tx = self.tx.clone();
        std::thread::spawn(move || {
            let _ = tx.send(Msg::OsChecked(github::fetch_os_release()));
        });
    }

    fn start_install(&mut self) {
        if self.install_busy || self.install_ok {
            return;
        }
        let Some(release) = self.os_latest.clone() else {
            self.install_status = "Check GitHub for the installer images first.".into();
            return;
        };
        let Some(system_asset) = github::system_asset(&release).cloned() else {
            self.install_status = "That release has no system image.".into();
            return;
        };
        let Some(boot_asset) = github::boot_asset(&release).cloned() else {
            self.install_status = "That release has no boot image.".into();
            return;
        };
        if system_asset.digest.is_none() || boot_asset.digest.is_none() {
            self.install_status = "The release doesn't publish SHA-256 digests; refusing to install it.".into();
            return;
        }
        let Some(disk) = self.disks.get(self.disk_sel) else {
            self.install_status = "Choose a target disk.".into();
            return;
        };
        if disk.target_ok == 0 {
            self.install_status = "That disk is too small for the install layout.".into();
            return;
        }
        if self.passphrase.len() < 8 {
            self.install_status = "Choose a passphrase of at least 8 characters.".into();
            return;
        }
        if self.passphrase != self.passphrase2 {
            self.install_status = "The two passphrases don't match. Type them again.".into();
            return;
        }
        let disk_index = self.disk_sel as u32;
        let passphrase = self.passphrase.clone();
        self.install_busy = true;
        self.install_status = format!("Downloading {} and {}...", boot_asset.name, system_asset.name);
        let tx = self.tx.clone();
        std::thread::spawn(move || {
            let result = (|| {
                let boot = github::fetch_bytes(&boot_asset.url, 64 * 1024 * 1024)?;
                github::verify_digest(&boot, boot_asset.digest.as_deref())?;
                let system = github::fetch_bytes(&system_asset.url, 128 * 1024 * 1024)?;
                github::verify_digest(&system, system_asset.digest.as_deref())?;
                sys::system_install(disk_index, &boot, &system, &passphrase).map_err(|e| e.to_string())
            })();
            let _ = tx.send(Msg::InstallFinished(result));
        });
    }

    fn start_update(&mut self) {
        if self.update_busy || self.update_done || self.netboot {
            return;
        }
        let Some(release) = self.os_latest.clone() else {
            self.update_status = "Check GitHub for an update first.".into();
            return;
        };
        let Some(asset) = github::system_asset(&release).cloned() else {
            self.update_status = "That release has no system image.".into();
            return;
        };
        if asset.digest.is_none() {
            self.update_status = "The release doesn't publish a SHA-256 digest; refusing to install it.".into();
            return;
        }
        self.update_busy = true;
        self.update_status = format!("Downloading {} ({})...", asset.name, format_size(asset.size));
        let tx = self.tx.clone();
        std::thread::spawn(move || {
            let result = (|| {
                let bytes = github::fetch_bytes(&asset.url, 128 * 1024 * 1024)?;
                github::verify_digest(&bytes, asset.digest.as_deref())?;
                sys::update_install(&bytes).map_err(|e| e.to_string())
            })();
            let _ = tx.send(Msg::UpdateFinished(result));
        });
    }

    fn click(&mut self, x: i32, y: i32, backend: &mut dyn Backend) {
        if let Some(p) = &self.pending_replace {
            if confirm_yes().contains(x, y) {
                let data = p.data.clone();
                let name = p.name.clone();
                match sys::app_install(&data, true) {
                    Ok(()) => {
                        self.status = format!("{name} was replaced.");
                        self.pending_replace = None;
                        self.refresh_apps();
                    }
                    Err(e) => self.status = format!("Replace failed: {e}"),
                }
            } else if confirm_no().contains(x, y) {
                self.pending_replace = None;
                self.status = "Kept the installed app.".into();
            }
            return;
        }
        if self.netboot {
            if tab_rect(0).contains(x, y) {
                self.tab = Tab::Os;
                return;
            }
        } else {
            for (i, tab) in [Tab::Apps, Tab::Updates, Tab::Os].iter().enumerate() {
                if tab_rect(i).contains(x, y) {
                    self.tab = *tab;
                    self.scroll = 0;
                    return;
                }
            }
        }
        match self.tab {
            Tab::Apps => {
                if check_all_rect().contains(x, y) && !self.checking {
                    self.check_all();
                    return;
                }
                if refresh_rect().contains(x, y) {
                    self.refresh_apps();
                    return;
                }
                if install_file_rect().contains(x, y) {
                    self.install_file(backend);
                    return;
                }
                let rows = self.apps.len();
                for i in self.scroll..rows.min(self.scroll + VISIBLE_ROWS) {
                    let row = &self.apps[i];
                    let y0 = row_y(i - self.scroll);
                    if row_check_rect(y0).contains(x, y) {
                        self.check_one(row.repo.clone());
                        return;
                    }
                    if row_action_rect(y0).contains(x, y) {
                        let has_asset = row.latest.as_ref().and_then(github::app_asset).is_some_and(|a| a.digest.is_some());
                        if has_asset {
                            self.install(row.repo.clone());
                        }
                        return;
                    }
                    if row_open_rect(y0).contains(x, y) {
                        let row = row.clone();
                        self.open(&row);
                        return;
                    }
                    if row_remove_rect(y0).contains(x, y) {
                        let row = row.clone();
                        self.remove(&row);
                        return;
                    }
                }
            }
            Tab::Updates => {
                if update_all_rect().contains(x, y) {
                    self.update_all();
                }
            }
            Tab::Os => {
                if self.netboot {
                    if os_check_rect().contains(x, y) {
                        self.check_os();
                        return;
                    }
                    for i in 0..self.disks.len().min(4) {
                        if disk_rect(i).contains(x, y) {
                            self.disk_sel = i;
                            return;
                        }
                    }
                    if pass_rect().contains(x, y) {
                        self.pass_focus = true;
                        self.pass_focus2 = false;
                        return;
                    }
                    if pass_rect2().contains(x, y) {
                        self.pass_focus2 = true;
                        self.pass_focus = false;
                        return;
                    }
                    self.pass_focus = false;
                    self.pass_focus2 = false;
                    if os_action_rect().contains(x, y) && !self.install_ok {
                        self.start_install();
                        return;
                    }
                    if reboot_rect().contains(x, y) && self.install_ok {
                        sys::reboot();
                    }
                } else {
                    if os_check_rect().contains(x, y) {
                        self.check_os();
                    } else if os_download_rect().contains(x, y) {
                        self.start_update();
                    } else if reboot_rect().contains(x, y) && self.update_done {
                        sys::reboot();
                    }
                }
            }
        }
    }

    fn draw(&mut self, c: &mut Canvas) {
        c.clear(C_BG);
        c.fill(0, 0, W as i32, 50, C_BAR);
        c.line(0, 49, W as i32, 1, 0x273154);
        let tabs: &[(&str, Tab)] = if self.netboot {
            &[("Install", Tab::Os)]
        } else {
            &[("Apps", Tab::Apps), ("Updates", Tab::Updates), ("OS", Tab::Os)]
        };
        for (i, (name, tab)) in tabs.iter().enumerate() {
            let r = tab_rect(i);
            let active = self.tab == *tab;
            c.round_rect(r.x, r.y, r.w, r.h, 7, if active { 0x244B5A } else if r.contains(self.mouse.0, self.mouse.1) { C_BTN_HOT } else { C_BAR });
            c.text(r.x + 16, r.y + 6, name, if active { C_ACCENT } else { C_DIM }, Font::Small);
        }
        match self.tab {
            Tab::Apps => self.draw_apps(c),
            Tab::Updates => self.draw_updates(c),
            Tab::Os => self.draw_os(c),
        }
        c.fill(0, H as i32 - 32, W as i32, 32, C_BAR);
        let footer = if self.netboot && !self.install_status.is_empty() {
            self.install_status.clone()
        } else if self.netboot {
            "Netboot installer: the target disk is untouched until Install.".to_string()
        } else {
            self.status.clone()
        };
        c.text(20, H as i32 - 25, &ellipsize(&footer, Font::Small, W as i32 - 40), C_DIM, Font::Small);
        if let Some(p) = &self.pending_replace {
            self.draw_confirm(c, p);
        }
    }

    fn draw_apps(&self, c: &mut Canvas) {
        c.text(20, 58, "Craft apps", C_TEXT, Font::Large);
        c.text(20, 102, "Packages come from github.com/traxsdata/Aether-CraftApps. The kernel verifies every package.", C_DIM, Font::Small);
        draw_button(c, check_all_rect(), "Check all", self.mouse, false, !self.checking);
        draw_button(c, refresh_rect(), "Refresh", self.mouse, false, true);
        let rows = self.apps.len();
        for i in self.scroll..rows.min(self.scroll + VISIBLE_ROWS) {
            let row = &self.apps[i];
            let y = row_y(i - self.scroll);
            let rect = Rect::new(20, y, W as i32 - 40, ROW_H);
            c.round_rect(rect.x, rect.y, rect.w, rect.h, 8, 0x121A30);
            if let Some(icon) = &row.icon {
                c.image(30, y + 12, 30, 30, &icon.rgba, icon.width, icon.height);
            } else {
                let (r, g, b) = ((row.color >> 16) as u8, (row.color >> 8) as u8, row.color as u8);
                c.round_rect(30, y + 12, 30, 30, 7, ((r as u32) << 16) | ((g as u32) << 8) | b as u32);
                let initial: String = row.title.chars().take(1).collect();
                let tw = text_width(&initial, Font::Medium);
                c.text(30 + (30 - tw) / 2, y + 17, &initial, 0xFFFFFF, Font::Medium);
            }
            c.text(72, y + 6, &row.title, C_TEXT, Font::Medium);
            let installed = row.installed_version();
            let latest = row.latest_version().unwrap_or_else(|| "not checked".into());
            let sub = format!(
                "{}  |  installed {}  |  latest {}{}",
                row.repo,
                if installed.is_empty() { "-" } else { &installed },
                latest,
                if row.pending() { "  | update pending" } else { "" }
            );
            c.text(72, y + 26, &ellipsize(&sub, Font::Small, 450), C_DIM, Font::Small);
            if !row.status.is_empty() {
                let color = if row.status.contains("No æther") { C_WARN } else { C_DIM };
                c.text(72, y + 42, &ellipsize(&row.status, Font::Small, 440), color, Font::Small);
            }
            draw_button(c, row_check_rect(y), "Check", self.mouse, false, !row.busy);
            let has_asset = row.latest.as_ref().and_then(github::app_asset).is_some_and(|a| a.digest.is_some());
            let label = if row.installed.is_some() {
                if row.update_available() { "Update" } else { "Reinstall" }
            } else {
                "Install"
            };
            draw_button(c, row_action_rect(y), label, self.mouse, false, !row.busy && has_asset);
            if row.installed.is_some() {
                draw_button(c, row_open_rect(y), "Open", self.mouse, false, true);
                draw_button(c, row_remove_rect(y), "Remove", self.mouse, false, true);
            }
        }
        if rows > VISIBLE_ROWS {
            let frac = self.scroll as f32 / (rows - VISIBLE_ROWS) as f32;
            let track_h = (LIST_BOTTOM - LIST_TOP) as i32;
            let h = (track_h * VISIBLE_ROWS as i32 / rows as i32).max(30);
            let y = LIST_TOP + (track_h - h) * frac as i32;
            c.round_rect(W as i32 - 14, y, 4, h, 2, 0x3A4A78);
        }
        c.line(20, 566, W as i32 - 40, 1, 0x273154);
        draw_button(c, install_file_rect(), "Install a package file...", self.mouse, false, true);
        c.text(240, 544, "Downloads require the release's SHA-256 digest.", C_DIM, Font::Small);
    }

    fn draw_updates(&self, c: &mut Canvas) {
        c.text(20, 64, "Updates", C_TEXT, Font::Large);
        c.text(20, 92, "Installing an update never restarts a running app; its title bar says update pending until it is next started.", C_DIM, Font::Small);
        let updates: Vec<&AppRow> = self.apps.iter().filter(|r| r.update_available() || r.pending()).collect();
        if updates.is_empty() {
            c.text(20, 140, "No updates found yet. Use Check all on the Apps page.", C_DIM, Font::Small);
            return;
        }
        let mut y = 130;
        for row in updates.iter().take(8) {
            c.round_rect(20, y, W as i32 - 40, 42, 8, 0x121A30);
            let installed = row.installed_version();
            c.text(32, y + 5, &row.title, C_TEXT, Font::Small);
            c.text(
                32,
                y + 23,
                &format!(
                    "{}  ->  {}{}",
                    if installed.is_empty() { "-" } else { &installed },
                    row.latest_version().unwrap_or_else(|| "?".into()),
                    if row.pending() { "  (pending restart)" } else { "" }
                ),
                C_DIM,
                Font::Small,
            );
            y += 48;
        }
        draw_button(c, update_all_rect(), "Update all", self.mouse, false, !self.installing);
    }

    fn draw_os(&self, c: &mut Canvas) {
        if self.netboot {
            self.draw_install(c);
        } else {
            self.draw_update(c);
        }
    }

    fn draw_install(&self, c: &mut Canvas) {
        c.text(20, 64, "Install æther", C_TEXT, Font::Large);
        c.text(20, 102, "This writes a new partition table. Everything on the chosen disk is erased.", C_WARN, Font::Small);
        c.text(20, 122, "The OS images come from the latest GitHub release and are SHA-256 checked.", C_DIM, Font::Small);

        if self.disks.is_empty() {
            c.text(20, 166, "No installable disk found.", C_ERR, Font::Medium);
            c.text(20, 194, "Connect the internal NVMe/SATA disk, then check again.", C_DIM, Font::Small);
        } else {
            c.text(20, 148, "Target disk", C_DIM, Font::Small);
            for (i, d) in self.disks.iter().take(4).enumerate() {
                let r = disk_rect(i);
                let sel = i == self.disk_sel;
                c.round_rect(r.x, r.y, r.w, r.h, 8, if sel { 0x244B5A } else { 0x121A30 });
                if sel {
                    c.round_outline(r.x, r.y, r.w, r.h, 8, C_ACCENT, 180);
                }
                let name = aether_host::abi::cstr(&d.name);
                let model = aether_host::abi::cstr(&d.model);
                c.text(r.x + 14, r.y + 7, &format!("{}  {}", name, model), C_TEXT, Font::Small);
                let gb = d.sectors as f64 * 512.0 / (1000.0 * 1000.0 * 1000.0);
                let note = if d.installed != 0 { "  |  already has æther (will be replaced)" } else { "" };
                c.text(r.x + 14, r.y + 27, &format!("{gb:.0} GB{note}"), if d.installed != 0 { C_WARN } else { C_DIM }, Font::Small);
            }
        }

        c.text(20, 382, "Disk passphrase (encrypts your files; at least 8 characters)", C_DIM, Font::Small);
        for (i, (label, focus, value)) in [
            ("passphrase", self.pass_focus, &self.passphrase),
            ("type it again", self.pass_focus2, &self.passphrase2),
        ]
        .iter()
        .enumerate()
        {
            let field = if i == 0 { pass_rect() } else { pass_rect2() };
            c.round_rect(field.x, field.y, field.w, field.h, 7, 0x121A30);
            if *focus {
                c.round_outline(field.x, field.y, field.w, field.h, 7, C_ACCENT, 180);
            }
            let shown = "*".repeat(value.chars().count());
            c.text(
                field.x + 10,
                field.y + 8,
                if shown.is_empty() { label } else { &shown },
                if shown.is_empty() { C_DIM } else { C_TEXT },
                Font::Small,
            );
        }

        draw_button(c, os_check_rect(), "Check GitHub", self.mouse, false, !self.os_busy && !self.install_busy);
        let ready = !self.install_busy && !self.install_ok && !self.disks.is_empty()
            && self.passphrase.len() >= 8 && self.passphrase == self.passphrase2
            && self.os_latest.as_ref().and_then(github::system_asset).is_some_and(|a| a.digest.is_some());
        draw_button(c, os_action_rect(), "Install æther", self.mouse, false, ready);
        if self.install_ok {
            draw_button(c, reboot_rect(), "Reboot", self.mouse, false, true);
        }

        if !self.install_status.is_empty() {
            c.text(20, 522, &ellipsize(&self.install_status, Font::Small, W as i32 - 40), C_TEXT, Font::Small);
        }
        if let Some(r) = &self.os_latest {
            c.text(20, 542, &format!("Release: {}", r.tag), C_DIM, Font::Small);
        }
        c.text(20, 562, "Keyboard: Tab moves between the passphrase fields, arrows choose a disk, Enter installs.", C_DIM, Font::Small);
        c.line(20, 584, W as i32 - 40, 1, 0x273154);
    }

    fn draw_update(&self, c: &mut Canvas) {
        c.text(20, 64, "æther OS", C_TEXT, Font::Large);
        c.text(20, 104, &format!("Installed: {}", aether_host::abi::cstr(&self.os.version)), C_TEXT, Font::Small);
        c.text(20, 126, &format!("Build: {}", aether_host::abi::cstr(&self.os.build)), C_DIM, Font::Small);
        if self.update.installed == 0 || self.update.boot_flags & BOOT_FLAG_INSTALLED == 0 {
            c.round_rect(20, 158, W as i32 - 40, 60, 8, 0x2A1A20);
            c.text(32, 170, "This system was booted from removable media.", C_WARN, Font::Medium);
            c.text(32, 194, "Use the netboot installer to install æther on a disk first.", C_DIM, Font::Small);
            draw_button(c, os_check_rect(), "Check for OS updates", self.mouse, false, !self.os_busy);
            if !self.os_status.is_empty() {
                c.text(20, 240, &ellipsize(&self.os_status, Font::Small, W as i32 - 40), C_TEXT, Font::Small);
            }
            return;
        }
        let running = aether_host::abi::cstr(&self.update.running_version);
        let other = aether_host::abi::cstr(&self.update.other_version);
        c.text(20, 158, &format!("Running slot {}: {}", self.update.running_slot, if running.is_empty() { "(unreadable)" } else { running }), C_TEXT, Font::Small);
        c.text(20, 180, &format!("Inactive slot {}: {}", 1 - self.update.running_slot, if other.is_empty() { "(empty)" } else { other }), C_DIM, Font::Small);
        if self.update.pending_slot != 0xFFFF_FFFF {
            c.text(20, 202, &format!("Pending boot: slot {} (attempt {} of 2)", self.update.pending_slot, self.update.attempts), C_WARN, Font::Small);
        } else {
            c.text(20, 202, "A/B rollback: a slot that can't reach the desktop is retried twice, then reverted.", C_DIM, Font::Small);
        }
        c.round_rect(20, 232, W as i32 - 40, 46, 8, 0x1A2434);
        c.text(32, 242, "Update images are checked against the release's SHA-256 digest.", C_TEXT, Font::Small);
        c.text(32, 260, "System images are signed by the release key; app packages are integrity-checked.", C_ACCENT, Font::Small);

        draw_button(c, os_check_rect(), "Check for OS updates", self.mouse, false, !self.os_busy && !self.update_busy);
        if let Some(r) = &self.os_latest {
            if let Some(a) = github::system_asset(r) {
                let ok = !self.os_busy && !self.update_busy && !self.update_done && a.digest.is_some();
                draw_button(c, os_action_rect(), &format!("Install {}", format_size(a.size)), self.mouse, false, ok);
            }
        }
        if self.update_done {
            draw_button(c, reboot_rect(), "Reboot", self.mouse, false, true);
        }
        if !self.os_status.is_empty() {
            c.text(20, 330, &ellipsize(&self.os_status, Font::Small, W as i32 - 40), C_TEXT, Font::Small);
        }
        if !self.update_status.is_empty() {
            c.text(20, 356, &ellipsize(&self.update_status, Font::Small, W as i32 - 40), C_TEXT, Font::Small);
        }
        c.text(20, 410, "The running slot and your files are never changed by an update.", C_DIM, Font::Small);
        c.text(20, 432, "The new slot must reach the desktop or the loader rolls back automatically.", C_DIM, Font::Small);
    }

    fn draw_confirm(&self, c: &mut Canvas, p: &PendingReplace) {
        c.blend(0, 0, W as i32, H as i32, 0x000000, 150);
        let r = Rect::new(170, 230, W as i32 - 340, 220);
        c.round_rect(r.x, r.y, r.w, r.h, 12, C_BAR);
        c.round_outline(r.x, r.y, r.w, r.h, 12, C_ACCENT, 120);
        c.text(r.x + 24, r.y + 22, "Replace the installed app?", C_TEXT, Font::Large);
        c.text(r.x + 24, r.y + 62, &format!("Name: {}", p.name), C_TEXT, Font::Small);
        c.text(r.x + 24, r.y + 88, &format!("Installed: {}", ellipsize(&p.old_source, Font::Small, r.w - 48)), C_DIM, Font::Small);
        c.text(r.x + 24, r.y + 110, &format!("New:       {}", ellipsize(&p.new_source, Font::Small, r.w - 48)), C_DIM, Font::Small);
        c.text(r.x + 24, r.y + 138, "A replacement inherits the old app's private folder and permissions.", C_WARN, Font::Small);
        c.text(r.x + 24, r.y + 158, "It cannot add a new capability without a fresh install.", C_DIM, Font::Small);
        draw_button(c, confirm_yes(), "Replace", self.mouse, false, true);
        draw_button(c, confirm_no(), "Cancel", self.mouse, false, true);
    }
}

impl App for Installer {
    fn draw(&mut self, canvas: &mut Canvas, _width: u32, _height: u32) {
        self.draw(canvas);
    }

    fn event(&mut self, event: &Event, backend: &mut dyn Backend) {
        match event {
            Event::MouseMove(m) => self.mouse = (m.x, m.y),
            Event::MouseDown(m) => {
                self.mouse = (m.x, m.y);
                self.click(m.x, m.y, backend);
            }
            Event::MouseUp(m) => self.mouse = (m.x, m.y),
            Event::Scroll { dy, .. } => {
                if self.tab == Tab::Apps {
                    let max = self.apps.len().saturating_sub(VISIBLE_ROWS);
                    let next = (self.scroll as i32 - *dy).clamp(0, max as i32);
                    self.scroll = next as usize;
                }
            }
            Event::Key { key, mods } => {
                if *mods & (MOD_CTRL | MOD_ALT) != 0 || !self.netboot {
                    return;
                }
                let enter = *key == '\n' as u32 || *key == '\r' as u32;
                if self.pass_focus || self.pass_focus2 {
                    let second = self.pass_focus2;
                    let field = if second { &mut self.passphrase2 } else { &mut self.passphrase };
                    if *key == 8 || *key == 127 {
                        field.pop();
                    } else if enter {
                        self.pass_focus = false;
                        self.pass_focus2 = false;
                    } else if *key == '\t' as u32 {
                        self.pass_focus = second;
                        self.pass_focus2 = !second;
                    } else if (32..127).contains(key) && field.len() < 64 {
                        field.push(char::from_u32(*key).unwrap_or('?'));
                    }
                    return;
                }
                if *key == '\t' as u32 {
                    self.pass_focus = true;
                } else if *key == KEY_UP {
                    self.disk_sel = self.disk_sel.saturating_sub(1);
                } else if *key == KEY_DOWN {
                    let max = self.disks.len().saturating_sub(1);
                    self.disk_sel = (self.disk_sel + 1).min(max);
                } else if enter {
                    if self.install_ok {
                        sys::reboot();
                    } else if self.os_latest.is_none() {
                        self.check_os();
                    } else {
                        self.start_install();
                    }
                }
            }
            _ => {}
        }
    }

    fn update(&mut self, _backend: &mut dyn Backend) -> bool {
        let mut changed = self.poll();
        if self.last_refresh.elapsed() >= Duration::from_secs(2) {
            self.refresh_apps();
            changed = true;
        }
        changed
    }
}

const ROW_H: i32 = 58;
const VISIBLE_ROWS: usize = 7;
const LIST_TOP: i32 = 124;
const LIST_BOTTOM: i32 = 530;

fn row_y(i: usize) -> i32 { LIST_TOP + (i as i32) * ROW_H }
fn tab_rect(i: usize) -> Rect { Rect::new(20 + i as i32 * 104, 12, 94, 28) }
fn check_all_rect() -> Rect { Rect::new(700, 62, 90, 28) }
fn refresh_rect() -> Rect { Rect::new(798, 62, 80, 28) }
fn install_file_rect() -> Rect { Rect::new(20, 536, 210, 30) }
fn row_check_rect(y: i32) -> Rect { Rect::new(548, y + 15, 64, 28) }
fn row_action_rect(y: i32) -> Rect { Rect::new(618, y + 15, 92, 28) }
fn row_open_rect(y: i32) -> Rect { Rect::new(716, y + 15, 70, 28) }
fn row_remove_rect(y: i32) -> Rect { Rect::new(792, y + 15, 84, 28) }
fn update_all_rect() -> Rect { Rect::new(20, 536, 110, 30) }
fn os_check_rect() -> Rect { Rect::new(20, 490, 150, 30) }
fn os_action_rect() -> Rect { Rect::new(180, 490, 220, 30) }
fn os_download_rect() -> Rect { os_action_rect() }
fn reboot_rect() -> Rect { Rect::new(410, 490, 100, 30) }
fn disk_rect(i: usize) -> Rect { Rect::new(20, 166 + i as i32 * 56, W as i32 - 40, 50) }
fn pass_rect() -> Rect { Rect::new(20, 406, 380, 32) }
fn pass_rect2() -> Rect { Rect::new(20, 446, 380, 32) }
fn confirm_yes() -> Rect { Rect::new(W as i32 - 370, 400, 100, 30) }
fn confirm_no() -> Rect { Rect::new(W as i32 - 260, 400, 100, 30) }

fn draw_button(c: &mut Canvas, r: Rect, label: &str, mouse: (i32, i32), selected: bool, enabled: bool) {
    let hot = enabled && r.contains(mouse.0, mouse.1);
    let fill = if selected { C_ACCENT } else if hot { C_BTN_HOT } else { C_BTN };
    let text = if selected { 0x06121A } else if enabled { C_TEXT } else { C_DIM };
    c.round_rect(r.x, r.y, r.w, r.h, 7, fill);
    let tw = text_width(label, Font::Small);
    c.text(r.x + (r.w - tw) / 2, r.y + (r.h - font_h(Font::Small)) / 2, label, text, Font::Small);
}

/// Refuse a catalog download whose package header is for a different app;
/// `replace` would otherwise let a compromised release replace another app.
fn check_package_name(data: &[u8], expected: &str) -> Result<(), String> {
    let Some((name, _, _)) = package_meta(data) else {
        return Err("The download isn't an æther app package.".into());
    };
    if name != expected {
        return Err(format!("The package is for {name}, not {expected}."));
    }
    Ok(())
}

fn package_meta(data: &[u8]) -> Option<(String, String, String)> {
    if data.len() < 512 || &data[..8] != b"AETHAPP1" {
        return None;
    }
    let field = |off: usize, n: usize| {
        let end = data[off..off + n].iter().position(|&b| b == 0).unwrap_or(n);
        String::from_utf8_lossy(&data[off..off + end]).into_owned()
    };
    let name = field(16, 32);
    if name.is_empty() {
        return None;
    }
    Some((name, field(80, 32), field(112, 96)))
}

/// One app's release, from the shared Craft release (or its fetch error).
fn craft_release(shared: &Result<Release, String>, repo: &str) -> Result<Release, String> {
    let shared = shared.as_ref().map_err(String::clone)?;
    github::app_release(shared, repo).ok_or_else(|| "No æther build published yet".into())
}

fn installed_matches(a: &app_info, repo: &str, title: &str) -> bool {
    let name = aether_host::abi::cstr(&a.name);
    let source = aether_host::abi::cstr(&a.source);
    let t = aether_host::abi::cstr(&a.title);
    name == repo || source.contains(repo) || t.eq_ignore_ascii_case(title)
}

fn format_size(n: u64) -> String {
    if n == 0 {
        "unknown size".into()
    } else if n < 1024 * 1024 {
        format!("{:.0} KB", n as f64 / 1024.0)
    } else {
        format!("{:.1} MB", n as f64 / (1024.0 * 1024.0))
    }
}

fn main() -> Result<(), String> {
    let netboot = sys::update_info().is_ok_and(|u| u.boot_flags & BOOT_FLAG_NETBOOT != 0);
    let mut app = Installer::new();
    system_ui::run(&mut app, W, H, if netboot { "Aether installer" } else { "App Store" })
}
