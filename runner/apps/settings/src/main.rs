//! Settings for æther 0.6: network, Wi-Fi status, firewall, privacy, about.
//!
//! This is a clean-room Rust system app: it draws with the kernel apps' own
//! software graphics and Facet font through `system-ui`, and uses only
//! `std`, `aether-host` and that in-tree binding. No egui, no third-party
//! crates and no bundled fonts.

use std::time::{Duration, Instant};

use aether_host::abi::*;
use aether_host::{sys, Backend, Event};
use system_ui::{ellipsize, font_h, text_width, App, Canvas, Font, Rect};

const W: u32 = 820;
const H: u32 = 620;

const C_BG: u32 = 0x0D1222;
const C_BAR: u32 = 0x131A31;
const C_FIELD: u32 = 0x0A0F1E;
const C_BTN: u32 = 0x1D2647;
const C_BTN_HOT: u32 = 0x2A3560;
const C_TEXT: u32 = 0xE2E8FA;
const C_DIM: u32 = 0x7C87A8;
const C_ACCENT: u32 = 0x38E1F5;
const C_OK: u32 = 0x5CF2B0;
const C_WARN: u32 = 0xFFB547;
const C_ERR: u32 = 0xFF5C7A;

#[derive(Clone, Copy, PartialEq, Eq)]
enum Tab {
    Network,
    Wifi,
    Firewall,
    Security,
    Privacy,
    About,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Focus {
    None,
    Net(usize),
    FwApp,
    FwHost,
    FwPort,
    PassOld,
    PassNew,
    PassAgain,
}

struct Settings {
    tab: Tab,
    net: net_config,
    net_error: String,
    net_notice: String,
    static_fields: [String; 4],
    net_edited: bool,
    wifi: wifi_info,
    rules: Vec<fw_rule>,
    fw_app: String,
    fw_host: String,
    fw_port: String,
    fw_allow: bool,
    fw_notice: String,
    autolock: u32,
    pass_old: String,
    pass_new: String,
    pass_again: String,
    security_notice: String,
    dlp: String,
    os: os_info,
    info: sys_info,
    fs: fs_info,
    last_refresh: Instant,
    mouse: (i32, i32),
    focus: Focus,
    scroll: i32,
}

impl Settings {
    fn new() -> Self {
        let mut s = Self {
            tab: Tab::Network,
            net: net_config::default(),
            net_error: String::new(),
            net_notice: String::new(),
            static_fields: Default::default(),
            net_edited: false,
            wifi: wifi_info::default(),
            rules: Vec::new(),
            fw_app: String::new(),
            fw_host: String::new(),
            fw_port: String::new(),
            fw_allow: true,
            fw_notice: String::new(),
            autolock: 10,
            pass_old: String::new(),
            pass_new: String::new(),
            pass_again: String::new(),
            security_notice: String::new(),
            dlp: String::new(),
            os: os_info::default(),
            info: sys_info::default(),
            fs: fs_info::default(),
            last_refresh: Instant::now() - Duration::from_secs(10),
            mouse: (0, 0),
            focus: Focus::None,
            scroll: 0,
        };
        s.refresh();
        s
    }

    fn refresh(&mut self) {
        if let Ok(c) = sys::net_config(NETCFG_GET, &net_config::default()) {
            self.net = c;
            if !self.net_edited {
                self.static_fields = [
                    ip_string(c.ip),
                    ip_string(c.netmask),
                    ip_string(c.gateway),
                    ip_string(c.dns),
                ];
            }
        }
        self.wifi = sys::wifi_info().unwrap_or_default();
        self.rules.clear();
        if let Ok(n) = sys::firewall_count() {
            for i in 0..n {
                if let Ok(r) = sys::firewall_get(i) {
                    self.rules.push(r);
                }
            }
        }
        let mut log = vec![0u8; 8192];
        self.dlp = match sys::dlp_log(&mut log) {
            Ok(n) => String::from_utf8_lossy(&log[..n]).into_owned(),
            Err(_) => "The audit log isn't available.".into(),
        };
        let mut v = [0u8; 16];
        self.autolock = match sys::settings_get("security.autolock", &mut v) {
            Ok(n) => String::from_utf8_lossy(&v[..n]).parse().unwrap_or(10),
            Err(_) => 10,
        };
        self.os = sys::os_info().unwrap_or_default();
        self.info = sys::sysinfo().unwrap_or_default();
        self.fs = sys::fs_info().unwrap_or_default();
        self.last_refresh = Instant::now();
    }

    fn apply_network(&mut self) {
        self.net_error.clear();
        self.net_notice.clear();
        if self.net.mode == NET_MODE_STATIC {
            let mut v = [0u32; 4];
            for (i, field) in self.static_fields.iter().enumerate() {
                match parse_ipv4(field.trim()) {
                    Some(ip) => v[i] = ip,
                    None => {
                        self.net_error = format!("'{}' isn't a valid IPv4 address", field);
                        return;
                    }
                }
            }
            self.net.ip = v[0];
            self.net.netmask = v[1];
            self.net.gateway = v[2];
            self.net.dns = v[3];
        }
        match sys::net_config(NETCFG_SET, &self.net) {
            Ok(c) => {
                self.net = c;
                self.net_edited = false;
                self.net_notice = "Saved. The new address is in use now.".into();
            }
            Err(e) => self.net_error = why(e),
        }
    }

    fn change_passphrase(&mut self) {
        self.security_notice.clear();
        if self.pass_new.len() < 8 {
            self.security_notice = "The new passphrase needs at least 8 characters.".into();
            return;
        }
        if self.pass_new != self.pass_again {
            self.security_notice = "The two new passphrases don't match.".into();
            return;
        }
        match sys::change_passphrase(&self.pass_old, &self.pass_new) {
            Ok(()) => {
                self.pass_old.clear();
                self.pass_new.clear();
                self.pass_again.clear();
                self.security_notice = "Passphrase changed. The disk key was re-wrapped.".into();
            }
            Err(e) => self.security_notice = why(e),
        }
    }

    fn add_rule(&mut self) {
        self.fw_notice.clear();
        let app = self.fw_app.trim();
        let host = self.fw_host.trim();
        if app.is_empty() || host.is_empty() {
            self.fw_notice = "An app name and a host are required.".into();
            return;
        }
        let port: u16 = if self.fw_port.trim().is_empty() {
            0
        } else {
            match self.fw_port.trim().parse() {
                Ok(p) => p,
                Err(_) => {
                    self.fw_notice = "The port must be a number (0 means any).".into();
                    return;
                }
            }
        };
        let mut r = fw_rule::default();
        copy_str(&mut r.app, app);
        copy_str(&mut r.host, host);
        r.port = port;
        r.action = if self.fw_allow { FW_ALLOW } else { FW_DENY };
        match sys::firewall_add(&r) {
            Ok(()) => {
                self.fw_notice = "Rule added.".into();
                self.fw_app.clear();
                self.fw_host.clear();
                self.fw_port.clear();
                self.refresh();
            }
            Err(e) => self.fw_notice = why(e),
        }
    }

    fn key(&mut self, key: u32, mods: u32) {
        if mods & (MOD_CTRL | MOD_ALT) != 0 {
            return;
        }
        if key == '\t' as u32 {
            self.focus = match self.focus {
                Focus::Net(i) => Focus::Net((i + 1) % 4),
                Focus::FwApp => Focus::FwHost,
                Focus::FwHost => Focus::FwPort,
                Focus::FwPort => Focus::FwApp,
                Focus::PassOld => Focus::PassNew,
                Focus::PassNew => Focus::PassAgain,
                Focus::PassAgain => Focus::PassOld,
                Focus::None => Focus::PassOld,
            };
            return;
        }
        let enter = key == '\n' as u32 || key == '\r' as u32;
        let back = key == 8 || key == 127;
        let ch = if (32..127).contains(&key) { Some(key as u8 as char) } else { None };
        match self.focus {
            Focus::Net(i) => {
                edit(&mut self.static_fields[i], ch, back, 15);
                self.net_edited = true;
            }
            Focus::FwApp => edit(&mut self.fw_app, ch, back, 15),
            Focus::FwHost => edit(&mut self.fw_host, ch, back, NET_HOST_MAX - 1),
            Focus::FwPort => edit(&mut self.fw_port, ch, back, 5),
            Focus::PassOld => edit(&mut self.pass_old, ch, back, 64),
            Focus::PassNew => edit(&mut self.pass_new, ch, back, 64),
            Focus::PassAgain => edit(&mut self.pass_again, ch, back, 64),
            Focus::None => {}
        }
        if enter {
            let was_net = matches!(self.focus, Focus::Net(_));
            let was_pass = matches!(self.focus, Focus::PassOld | Focus::PassNew | Focus::PassAgain);
            self.focus = Focus::None;
            if was_net {
                self.apply_network();
            } else if was_pass {
                self.change_passphrase();
            }
        }
    }

    fn click(&mut self, x: i32, y: i32) {
        for (i, tab) in [Tab::Network, Tab::Wifi, Tab::Firewall, Tab::Security, Tab::Privacy, Tab::About]
            .iter()
            .enumerate()
        {
            if tab_rect(i).contains(x, y) {
                self.tab = *tab;
                self.focus = Focus::None;
                self.scroll = 0;
                return;
            }
        }
        match self.tab {
            Tab::Network => {
                if net_mode_rect(0).contains(x, y) {
                    self.net.mode = NET_MODE_DHCP;
                    self.net_notice.clear();
                } else if net_mode_rect(1).contains(x, y) {
                    self.net.mode = NET_MODE_STATIC;
                    self.net_notice.clear();
                } else if self.net.mode == NET_MODE_STATIC {
                    for i in 0..4 {
                        if net_field_rect(i).contains(x, y) {
                            self.focus = Focus::Net(i);
                            return;
                        }
                    }
                    if apply_rect().contains(x, y) {
                        self.apply_network();
                    }
                } else if apply_rect().contains(x, y) {
                    self.apply_network();
                }
            }
            Tab::Firewall => {
                for i in 0..self.rules.len().saturating_sub(self.scroll.max(0) as usize).min(7) {
                    if fw_remove_rect(i).contains(x, y) {
                        if let Err(e) = sys::firewall_remove((self.scroll.max(0) as usize + i) as u32) {
                            self.fw_notice = why(e);
                        }
                        self.refresh();
                        return;
                    }
                }
                if fw_field_rect(0).contains(x, y) {
                    self.focus = Focus::FwApp;
                } else if fw_field_rect(1).contains(x, y) {
                    self.focus = Focus::FwHost;
                } else if fw_field_rect(2).contains(x, y) {
                    self.focus = Focus::FwPort;
                } else if fw_allow_rect().contains(x, y) {
                    self.fw_allow = !self.fw_allow;
                } else if fw_add_rect().contains(x, y) {
                    self.add_rule();
                }
            }
            Tab::Security => {
                for (i, mins) in [0u32, 5, 10, 30].iter().enumerate() {
                    if sec_auto_rect(i).contains(x, y) {
                        self.autolock = *mins;
                        self.security_notice.clear();
                        if let Err(e) = sys::settings_set("security.autolock", &mins.to_string()) {
                            self.security_notice = why(e);
                        } else if *mins == 0 {
                            self.security_notice = "The desktop will not lock by itself.".into();
                        } else {
                            self.security_notice = format!("Auto-lock set to {mins} minutes.");
                        }
                        return;
                    }
                }
                if lock_now_rect().contains(x, y) {
                    match sys::lock_screen() {
                        Ok(()) => self.security_notice = "Locking...".into(),
                        Err(e) => self.security_notice = why(e),
                    }
                } else if pass_field_rect(0).contains(x, y) {
                    self.focus = Focus::PassOld;
                } else if pass_field_rect(1).contains(x, y) {
                    self.focus = Focus::PassNew;
                } else if pass_field_rect(2).contains(x, y) {
                    self.focus = Focus::PassAgain;
                } else if pass_apply_rect().contains(x, y) {
                    self.change_passphrase();
                }
            }
            Tab::Privacy => {}
            Tab::Wifi | Tab::About => {}
        }
    }

    fn draw(&mut self, c: &mut Canvas) {
        c.clear(C_BG);
        c.fill(0, 0, W as i32, 50, C_BAR);
        c.line(0, 49, W as i32, 1, 0x273154);
        for (i, name) in ["Network", "Wi-Fi", "Firewall", "Security", "Privacy", "About"].iter().enumerate() {
            let r = tab_rect(i);
            let active = matches!(
                (self.tab, i),
                (Tab::Network, 0) | (Tab::Wifi, 1) | (Tab::Firewall, 2) | (Tab::Security, 3)
                    | (Tab::Privacy, 4) | (Tab::About, 5)
            );
            c.round_rect(r.x, r.y, r.w, r.h, 7, if active { 0x244B5A } else if r.contains(self.mouse.0, self.mouse.1) { C_BTN_HOT } else { C_BAR });
            c.text(r.x + 12, r.y + 6, name, if active { C_ACCENT } else { C_DIM }, Font::Small);
        }
        match self.tab {
            Tab::Network => self.draw_network(c),
            Tab::Wifi => self.draw_wifi(c),
            Tab::Firewall => self.draw_firewall(c),
            Tab::Security => self.draw_security(c),
            Tab::Privacy => self.draw_privacy(c),
            Tab::About => self.draw_about(c),
        }
        let status = if !self.net_error.is_empty() {
            Some((C_ERR, self.net_error.clone()))
        } else if !self.net_notice.is_empty() {
            Some((C_OK, self.net_notice.clone()))
        } else if !self.fw_notice.is_empty() {
            Some((C_WARN, self.fw_notice.clone()))
        } else if !self.security_notice.is_empty() {
            Some((C_OK, self.security_notice.clone()))
        } else {
            None
        };
        if let Some((color, text)) = status {
            c.fill(0, H as i32 - 30, W as i32, 30, C_BAR);
            c.text(20, H as i32 - 23, &ellipsize(&text, Font::Small, W as i32 - 40), color, Font::Small);
        }
    }

    fn draw_network(&self, c: &mut Canvas) {
        c.text(24, 66, "Network", C_TEXT, Font::Large);
        c.text(24, 100, "Mode", C_DIM, Font::Small);
        draw_button(c, net_mode_rect(0), "Automatic (DHCP)", self.mouse, self.net.mode == NET_MODE_DHCP, true);
        draw_button(c, net_mode_rect(1), "Static", self.mouse, self.net.mode == NET_MODE_STATIC, true);
        let mut y = 142;
        if self.net.mode == NET_MODE_STATIC {
            for (i, label) in ["Address", "Netmask", "Gateway", "DNS"].iter().enumerate() {
                c.text(24, y + 6, label, C_DIM, Font::Small);
                draw_field(c, net_field_rect(i), &self.static_fields[i], "0.0.0.0", self.focus == Focus::Net(i), self.mouse);
                y += 34;
            }
            draw_button(c, apply_rect(), "Apply", self.mouse, false, true);
        } else {
            c.text(24, 142, &format!("Address   {}", ip_string(self.net.ip)), C_TEXT, Font::Small);
            c.text(24, 166, &format!("Netmask   {}", ip_string(self.net.netmask)), C_TEXT, Font::Small);
            c.text(24, 190, &format!("Gateway   {}", ip_string(self.net.gateway)), C_TEXT, Font::Small);
            c.text(24, 214, &format!("DNS       {}", ip_string(self.net.dns)), C_TEXT, Font::Small);
            draw_button(c, apply_rect(), "Renew DHCP", self.mouse, false, true);
        }
        c.text(24, 340, &format!("MAC       {}", mac_string()), C_DIM, Font::Small);
        c.text(
            24,
            372,
            "Every outgoing connection is still checked by the firewall.",
            C_DIM,
            Font::Small,
        );
        c.text(24, 392, "æther never accepts incoming connections.", C_DIM, Font::Small);
    }

    fn draw_wifi(&self, c: &mut Canvas) {
        c.text(24, 66, "Wi-Fi", C_TEXT, Font::Large);
        if self.wifi.present == 0 {
            c.text(24, 108, "No Wi-Fi adapter found.", C_WARN, Font::Medium);
            c.text(24, 144, "æther 0.6 has a driver for the Intel wired cards VMware and QEMU emulate.", C_DIM, Font::Small);
            c.text(24, 164, "Wi-Fi drivers for real laptops are planned next.", C_DIM, Font::Small);
            c.text(24, 196, "The Network page still works over a wired connection.", C_DIM, Font::Small);
        } else {
            let ssid = aether_host::abi::cstr(&self.wifi.ssid);
            c.text(24, 108, if self.wifi.connected == 1 { "Connected" } else { "Not connected" }, C_TEXT, Font::Medium);
            if !ssid.is_empty() {
                c.text(24, 140, &format!("Network: {ssid}"), C_TEXT, Font::Small);
            }
            if self.wifi.connected == 1 {
                c.text(24, 164, &format!("Signal: {} dBm", self.wifi.signal), C_DIM, Font::Small);
            }
        }
        let detail = aether_host::abi::cstr(&self.wifi.detail);
        if !detail.is_empty() {
            c.text(24, 230, detail, C_DIM, Font::Small);
        }
    }

    fn draw_firewall(&self, c: &mut Canvas) {
        c.text(24, 66, "Firewall", C_TEXT, Font::Large);
        c.text(24, 98, "First matching rule wins. Your rules override the built-in web policy.", C_DIM, Font::Small);
        c.text(24, 116, "Built-in: Browser may reach any public host on 443 and 80 without asking.", C_WARN, Font::Small);
        let mut y = 144;
        c.text(24, y, "App", C_DIM, Font::Small);
        c.text(130, y, "Host", C_DIM, Font::Small);
        c.text(330, y, "Port", C_DIM, Font::Small);
        c.text(392, y, "Action", C_DIM, Font::Small);
        y += 22;
        for (i, r) in self.rules.iter().skip(self.scroll.max(0) as usize).take(7).enumerate() {
            let row = Rect::new(20, y - 3, 760, 26);
            if row.contains(self.mouse.0, self.mouse.1) {
                c.fill(row.x, row.y, row.w, row.h, 0x161E38);
            }
            c.text(24, y, aether_host::abi::cstr(&r.app), C_TEXT, Font::Small);
            c.text(130, y, &ellipsize(aether_host::abi::cstr(&r.host), Font::Small, 190), C_TEXT, Font::Small);
            let port = if r.port == 0 { "any".to_string() } else { r.port.to_string() };
            c.text(330, y, &port, C_TEXT, Font::Small);
            c.text(392, y, if r.action == FW_ALLOW { "Allow" } else { "Block" }, if r.action == FW_ALLOW { C_OK } else { C_ERR }, Font::Small);
            draw_button(c, fw_remove_rect(i), "Remove", self.mouse, false, true);
            y += 30;
        }
        let form_y = 380;
        if self.rules.len() > 7 {
            c.text(24, form_y - 40, &format!("{} rules; showing {} of them (scroll to see more).", self.rules.len(), 7), C_DIM, Font::Small);
        }
        c.text(24, form_y - 22, "Add a rule", C_DIM, Font::Small);
        draw_field(c, fw_field_rect(0), &self.fw_app, "app", self.focus == Focus::FwApp, self.mouse);
        draw_field(c, fw_field_rect(1), &self.fw_host, "example.com or *", self.focus == Focus::FwHost, self.mouse);
        draw_field(c, fw_field_rect(2), &self.fw_port, "any", self.focus == Focus::FwPort, self.mouse);
        draw_button(c, fw_allow_rect(), if self.fw_allow { "Allow" } else { "Block" }, self.mouse, self.fw_allow, true);
        draw_button(c, fw_add_rect(), "Add", self.mouse, false, true);
    }

    fn draw_security(&self, c: &mut Canvas) {
        c.text(24, 66, "Screen lock", C_TEXT, Font::Large);
        c.text(24, 100, "Lock the desktop when it has been idle. Unlocking needs your file passphrase.", C_DIM, Font::Small);
        c.text(24, 136, "Auto-lock", C_TEXT, Font::Medium);
        for (i, (label, mins)) in [("Never", 0u32), ("5 minutes", 5), ("10 minutes", 10), ("30 minutes", 30)]
            .iter()
            .enumerate()
        {
            let r = sec_auto_rect(i);
            draw_button(c, r, label, self.mouse, self.autolock == *mins, true);
        }
        c.text(24, 204, "You can also lock at any time with Super+L or the æ menu.", C_DIM, Font::Small);
        draw_button(c, lock_now_rect(), "Lock Now", self.mouse, false, true);
        c.text(24, 286, "While locked, apps keep running but receive no input, and the", C_DIM, Font::Small);
        c.text(24, 306, "desktop is not drawn. Your files stay encrypted on disk.", C_DIM, Font::Small);
        c.text(24, 348, "Security status", C_TEXT, Font::Medium);
        c.text(
            24,
            374,
            &format!("Files: {}", if self.fs.kind == FS_ENCRYPTED { "encrypted" } else { "temporary (no passphrase)" }),
            if self.fs.kind == FS_ENCRYPTED { C_OK } else { C_WARN },
            Font::Small,
        );
        c.text(24, 396, "Kernel: W^X, SMEP/SMAP when the CPU supports them, no app hardware access.", C_DIM, Font::Small);
        c.text(24, 418, "Network: incoming connections refused; outgoing connections pass the firewall.", C_DIM, Font::Small);

        c.text(24, 448, "Change passphrase", C_TEXT, Font::Medium);
        c.text(24, 468, "Current", C_DIM, Font::Small);
        c.text(204, 468, "New (8+ characters)", C_DIM, Font::Small);
        c.text(384, 468, "New again", C_DIM, Font::Small);
        draw_field(c, pass_field_rect(0), &self.pass_old, "current passphrase", self.focus == Focus::PassOld, self.mouse);
        draw_field(c, pass_field_rect(1), &self.pass_new, "new passphrase", self.focus == Focus::PassNew, self.mouse);
        draw_field(c, pass_field_rect(2), &self.pass_again, "repeat it", self.focus == Focus::PassAgain, self.mouse);
        draw_button(c, pass_apply_rect(), "Change", self.mouse, false, true);
        c.text(24, 512, "The old passphrase is checked and the volume key is re-wrapped; neither is stored.", C_DIM, Font::Small);
    }

    fn draw_privacy(&self, c: &mut Canvas) {
        c.text(24, 66, "Privacy and data", C_TEXT, Font::Large);
        let kind = match self.fs.kind {
            FS_ENCRYPTED => "Encrypted volume",
            FS_TEMPORARY => "Temporary (in memory, lost at shutdown)",
            _ => "No storage",
        };
        c.text(24, 104, &format!("Storage: {kind}"), C_TEXT, Font::Small);
        if self.fs.kind == FS_ENCRYPTED {
            let total = self.fs.total_blocks as f64 * 4.0 / 1024.0;
            let free = self.fs.free_blocks as f64 * 4.0 / 1024.0;
            c.text(24, 126, &format!("Free space: {free:.1} MB of {total:.1} MB"), C_DIM, Font::Small);
        }
        c.text(24, 148, &format!("This app holds: {}", label_name(self.fs.taint)), C_DIM, Font::Small);
        c.text(24, 180, "DLP audit log", C_TEXT, Font::Medium);
        let line_h = font_h(Font::Small) + 2;
        let rows = ((H as i32 - 220) / line_h).max(1) as usize;
        let lines: Vec<&str> = if self.dlp.is_empty() { vec!["No events yet."] } else { self.dlp.lines().collect() };
        let start = (self.scroll.max(0) as usize).min(lines.len().saturating_sub(1));
        let mut y = 208;
        for line in lines.iter().skip(start).take(rows) {
            c.text(24, y, &ellipsize(line, Font::Small, W as i32 - 48), C_DIM, Font::Small);
            y += line_h;
        }
    }

    fn draw_about(&self, c: &mut Canvas) {
        c.text(24, 66, "About æther", C_TEXT, Font::Large);
        c.text(24, 104, &format!("æther {}", aether_host::abi::cstr(&self.os.version)), C_TEXT, Font::Medium);
        c.text(24, 130, &format!("Build: {}", aether_host::abi::cstr(&self.os.build)), C_DIM, Font::Small);
        c.text(24, 152, &format!("Uptime: {} min", self.os.uptime_ms / 60000), C_DIM, Font::Small);
        c.text(24, 184, &format!("CPU: {}", aether_host::abi::cstr(&self.info.cpu_brand)), C_TEXT, Font::Small);
        c.text(
            24,
            206,
            &format!("Memory: {} MiB free of {} MiB", self.info.mem_free_kib / 1024, self.info.mem_total_kib / 1024),
            C_TEXT,
            Font::Small,
        );
        c.text(24, 228, &format!("Processes: {}", self.info.nproc), C_TEXT, Font::Small);
        c.text(24, 250, &format!("Screen: {}x{}", self.info.screen_w, self.info.screen_h), C_TEXT, Font::Small);
        c.text(24, 282, "Protections:", C_TEXT, Font::Small);
        let mut y = 304;
        for (bit, name, what) in [
            (CPU_FEAT_NX, "NX", "data can't be executed"),
            (CPU_FEAT_SMEP, "SMEP", "kernel can't execute app memory"),
            (CPU_FEAT_SMAP, "SMAP", "kernel can't read app memory by accident"),
            (CPU_FEAT_UMIP, "UMIP", "apps can't read kernel table addresses"),
            (CPU_FEAT_RDRAND, "RDRAND", "hardware randomness"),
        ] {
            let on = self.info.cpu_features & bit != 0;
            c.text(24, y, &format!("[{}] {name}: {what}", if on { "on" } else { "n/a" }), if on { C_OK } else { C_WARN }, Font::Small);
            y += 22;
        }
        c.text(24, 444, "A from-scratch operating system with security as its foundation.", C_DIM, Font::Small);
    }
}

impl App for Settings {
    fn draw(&mut self, canvas: &mut Canvas, _width: u32, _height: u32) {
        self.draw(canvas);
    }

    fn event(&mut self, event: &Event, _backend: &mut dyn Backend) {
        match event {
            Event::MouseMove(m) => self.mouse = (m.x, m.y),
            Event::MouseDown(m) => {
                self.mouse = (m.x, m.y);
                self.click(m.x, m.y);
            }
            Event::MouseUp(m) => self.mouse = (m.x, m.y),
            Event::Key { key, mods } => self.key(*key, *mods),
            Event::Scroll { dy, .. } => {
                if self.tab == Tab::Privacy {
                    self.scroll = (self.scroll - *dy).max(0);
                } else if self.tab == Tab::Firewall {
                    let max = self.rules.len().saturating_sub(7) as i32;
                    self.scroll = (self.scroll - *dy).clamp(0, max);
                }
            }
            _ => {}
        }
    }

    fn update(&mut self, _backend: &mut dyn Backend) -> bool {
        if self.last_refresh.elapsed() >= Duration::from_secs(2) {
            self.refresh();
            true
        } else {
            false
        }
    }
}

fn tab_rect(i: usize) -> Rect { Rect::new(16 + i as i32 * 92, 12, 84, 28) }
fn net_mode_rect(i: usize) -> Rect { Rect::new(24 + i as i32 * 174, 96, 160, 28) }
fn net_field_rect(i: usize) -> Rect { Rect::new(160, 136 + i as i32 * 34, 200, 26) }
fn apply_rect() -> Rect { Rect::new(24, 286, 110, 28) }
fn fw_field_rect(i: usize) -> Rect {
    match i {
        0 => Rect::new(24, 380, 90, 28),
        1 => Rect::new(124, 380, 180, 28),
        _ => Rect::new(314, 380, 60, 28),
    }
}
fn fw_allow_rect() -> Rect { Rect::new(384, 380, 76, 28) }
fn fw_add_rect() -> Rect { Rect::new(470, 380, 60, 28) }
fn fw_remove_rect(i: usize) -> Rect { Rect::new(720, 161 + i as i32 * 30, 60, 24) }
fn sec_auto_rect(i: usize) -> Rect { Rect::new(24 + i as i32 * 112, 164, 104, 28) }
fn lock_now_rect() -> Rect { Rect::new(24, 236, 120, 30) }
fn pass_field_rect(i: usize) -> Rect { Rect::new(24 + i as i32 * 180, 484, 170, 28) }
fn pass_apply_rect() -> Rect { Rect::new(564, 484, 100, 28) }

fn draw_button(c: &mut Canvas, r: Rect, label: &str, mouse: (i32, i32), selected: bool, enabled: bool) {
    let hot = enabled && r.contains(mouse.0, mouse.1);
    let fill = if selected {
        C_ACCENT
    } else if hot {
        C_BTN_HOT
    } else {
        C_BTN
    };
    let text = if selected { 0x06121A } else if enabled { C_TEXT } else { C_DIM };
    c.round_rect(r.x, r.y, r.w, r.h, 7, fill);
    let tw = text_width(label, Font::Small);
    c.text(r.x + (r.w - tw) / 2, r.y + (r.h - font_h(Font::Small)) / 2, label, text, Font::Small);
}

fn draw_field(c: &mut Canvas, r: Rect, value: &str, placeholder: &str, focused: bool, mouse: (i32, i32)) {
    let hot = r.contains(mouse.0, mouse.1);
    c.round_rect(r.x, r.y, r.w, r.h, 6, C_FIELD);
    c.round_outline(r.x, r.y, r.w, r.h, 6, if focused { C_ACCENT } else if hot { 0x3A4A78 } else { 0x273154 }, 255);
    let shown = if value.is_empty() { placeholder } else { value };
    let color = if value.is_empty() { 0x4A5578 } else { C_TEXT };
    c.text(r.x + 8, r.y + 5, &ellipsize(shown, Font::Small, r.w - 16), color, Font::Small);
    if focused {
        let cx = r.x + 8 + text_width(value, Font::Small).min(r.w - 12);
        c.line(cx, r.y + 5, 1, r.h - 10, C_ACCENT);
    }
}

fn edit(s: &mut String, ch: Option<char>, back: bool, max: usize) {
    if back {
        s.pop();
    } else if let Some(c) = ch {
        if s.len() < max {
            s.push(c);
        }
    }
}

fn parse_ipv4(s: &str) -> Option<u32> {
    let parts: Vec<&str> = s.split('.').collect();
    if parts.len() != 4 {
        return None;
    }
    let mut b = [0u8; 4];
    for (i, p) in parts.iter().enumerate() {
        b[i] = p.parse().ok()?;
    }
    Some(u32::from_ne_bytes(b))
}

fn ip_string(ip: u32) -> String {
    let b = ip.to_ne_bytes();
    format!("{}.{}.{}.{}", b[0], b[1], b[2], b[3])
}

fn mac_string() -> String {
    match sys::net_info() {
        Ok(n) if n.link != 0 => format!(
            "{:02X}:{:02X}:{:02X}:{:02X}:{:02X}:{:02X}",
            n.mac[0], n.mac[1], n.mac[2], n.mac[3], n.mac[4], n.mac[5]
        ),
        _ => "no wired card".into(),
    }
}

fn copy_str(dst: &mut [u8], s: &str) {
    let n = s.len().min(dst.len().saturating_sub(1));
    dst[..n].copy_from_slice(&s.as_bytes()[..n]);
    for b in &mut dst[n..] {
        *b = 0;
    }
}

fn label_name(label: u32) -> &'static str {
    match label {
        0 => "Public",
        1 => "Personal",
        2 => "Confidential",
        3 => "Secret",
        _ => "?",
    }
}

fn why(e: aether_host::Error) -> String {
    match e.0 {
        E_PERM => "missing capability".into(),
        E_INVAL => "invalid value".into(),
        E_NOTSUP => "not supported by this hardware".into(),
        E_NET => "network error".into(),
        E_LOCKED => "storage is locked or temporary".into(),
        E_DENIED => "wrong current passphrase, or refused".into(),
        E_LIMIT => "too many rules".into(),
        _ => format!("error: {e}"),
    }
}

fn main() -> Result<(), String> {
    let mut app = Settings::new();
    system_ui::run(&mut app, W, H, "Settings")
}
