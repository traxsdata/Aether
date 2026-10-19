//! abi.rs - Rust copies of the parts of include/abi.h the runner uses.
//!
//! Layouts are `#[repr(C)]` and must stay identical to the C structs; the
//! size checks at the bottom fail the build if they drift.

#![allow(non_camel_case_types)]

/// From the Linux-ABI layer, native call N is `syscall` with rax = 0x1000 + N.
pub const NATIVE_BASE: u64 = 0x1000;

pub const SYS_EXIT: u64 = 0;
pub const SYS_LOG: u64 = 1;
pub const SYS_WIN_CREATE: u64 = 2;
pub const SYS_WIN_PRESENT: u64 = 3;
pub const SYS_EVENT_WAIT: u64 = 4;
pub const SYS_TIME: u64 = 5;
pub const SYS_SYSINFO: u64 = 6;
pub const SYS_SPAWN: u64 = 8;
pub const SYS_WAIT: u64 = 12;
pub const SYS_WIN_TITLE: u64 = 13;
pub const SYS_OPEN: u64 = 15;
pub const SYS_CLOSE: u64 = 16;
pub const SYS_READ: u64 = 17;
pub const SYS_WRITE: u64 = 18;
pub const SYS_SEEK: u64 = 19;
pub const SYS_STAT: u64 = 20;
pub const SYS_READDIR: u64 = 21;
pub const SYS_MKDIR: u64 = 22;
pub const SYS_REMOVE: u64 = 23;
pub const SYS_RENAME: u64 = 24;
pub const SYS_SET_LABEL: u64 = 25;
pub const SYS_PICK_FILE: u64 = 26;
pub const SYS_FS_INFO: u64 = 27;
pub const SYS_DLP_LOG: u64 = 28;
pub const SYS_MEM_MAP: u64 = 29;
pub const SYS_MEM_UNMAP: u64 = 30;
pub const SYS_MEM_PROTECT: u64 = 31;
pub const SYS_DISPLAY_INFO: u64 = 32;
pub const SYS_EVENT_WAKE: u64 = 33;
pub const SYS_NET_CONNECT: u64 = 34;
pub const SYS_NET_SEND: u64 = 35;
pub const SYS_NET_RECV: u64 = 36;
pub const SYS_NET_CLOSE: u64 = 37;
pub const SYS_NET_INFO: u64 = 38;
pub const SYS_FIREWALL: u64 = 39;
pub const SYS_APP_LIST: u64 = 41;
pub const SYS_APP_INSTALL: u64 = 42;
pub const SYS_APP_REMOVE: u64 = 43;
pub const SYS_APP_INFO: u64 = 44;
pub const SYS_APP_ICON: u64 = 50;
pub const SYS_DISK_LIST: u64 = 51;
pub const SYS_SYSTEM_INSTALL: u64 = 52;
pub const SYS_UPDATE_INFO: u64 = 53;
pub const SYS_UPDATE_INSTALL: u64 = 54;
pub const SYS_REBOOT: u64 = 55;
pub const SYS_WIN_FLAGS: u64 = 56;
pub const SYS_LOCK: u64 = 58;
pub const SYS_TRUNCATE: u64 = 59;
pub const SYS_CHANGE_PASSPHRASE: u64 = 60;
pub const SYS_SHUTDOWN: u64 = 61;
pub const SYS_WIN_RESIZE: u64 = 57;
pub const SYS_NET_CONFIG: u64 = 45;
pub const SYS_SETTINGS_GET: u64 = 46;
pub const SYS_SETTINGS_SET: u64 = 47;
pub const SYS_OS_INFO: u64 = 48;
pub const SYS_WIFI_INFO: u64 = 49;

pub const E_PERM: i32 = 1;
pub const E_FAULT: i32 = 2;
pub const E_INVAL: i32 = 3;
pub const E_NOMEM: i32 = 4;
pub const E_NOENT: i32 = 5;
pub const E_LIMIT: i32 = 6;
pub const E_TIMEOUT: i32 = 7;
pub const E_EXIST: i32 = 8;
pub const E_NOTDIR: i32 = 9;
pub const E_ISDIR: i32 = 10;
pub const E_NOSPACE: i32 = 11;
pub const E_IO: i32 = 12;
pub const E_DENIED: i32 = 13;
pub const E_LOCKED: i32 = 14;
pub const E_NOTEMPTY: i32 = 15;
/// No network, the host can't be found, or the connection was refused or reset.
pub const E_NET: i32 = 16;
pub const E_BUSY: i32 = 17;
pub const E_NOTSUP: i32 = 18;

/// Capability bits.
pub const CAP_WINDOW: u32 = 1 << 0;
pub const CAP_LOG: u32 = 1 << 1;
pub const CAP_SYSINFO: u32 = 1 << 2;
pub const CAP_SPAWN: u32 = 1 << 3;
pub const CAP_KILL: u32 = 1 << 4;
pub const CAP_FILES: u32 = 1 << 5;
pub const CAP_PICK: u32 = 1 << 6;
pub const CAP_STORAGE: u32 = 1 << 7;
pub const CAP_NET: u32 = 1 << 8;
pub const CAP_FIREWALL: u32 = 1 << 9;
pub const CAP_APPS: u32 = 1 << 10;
pub const CAP_CONFIG: u32 = 1 << 11;
pub const CAP_UPDATE: u32 = 1 << 12;
pub const NET_HOST_MAX: usize = 64;

pub const NAME_MAX: usize = 58;
pub const PATH_MAX: usize = 256;
pub const O_READ: u32 = 1;
pub const O_WRITE: u32 = 2;
pub const O_CREATE: u32 = 4;
pub const O_TRUNC: u32 = 8;
pub const O_APPEND: u32 = 16;
pub const O_DIR: u32 = 32;
pub const O_EXCL: u32 = 64;
pub const PROT_READ: u32 = 1;
pub const PROT_WRITE: u32 = 2;
pub const PROT_EXEC: u32 = 4;
pub const SEEK_SET: u32 = 0;
pub const SEEK_CUR: u32 = 1;
pub const SEEK_END: u32 = 2;
pub const FT_FILE: u32 = 1;
pub const FT_DIR: u32 = 2;

pub const LABEL_PUBLIC: u32 = 0;
pub const LABEL_PERSONAL: u32 = 1;
pub const LABEL_CONFIDENTIAL: u32 = 2;
pub const LABEL_SECRET: u32 = 3;

pub const PICK_OPEN: u32 = 1;
pub const PICK_SAVE: u32 = 2;

pub const FS_NONE: u32 = 0;
pub const FS_TEMPORARY: u32 = 1;
pub const FS_ENCRYPTED: u32 = 2;

pub const WIN_MAX_W: u32 = 1600;
pub const WIN_MAX_H: u32 = 1000;
pub const WIN_TITLE_MAX: usize = 40;
pub const WIN_MIN_W: u32 = 160;
pub const WIN_MIN_H: u32 = 100;
/// The app redraws at any size it is given (EV_RESIZE).
pub const WIN_FLAG_RESIZABLE: u32 = 1;

pub const EV_KEY: u32 = 1;
pub const EV_MOUSE_MOVE: u32 = 2;
pub const EV_MOUSE_DOWN: u32 = 3;
pub const EV_MOUSE_UP: u32 = 4;
pub const EV_CLOSE: u32 = 5;
pub const EV_FOCUS: u32 = 6;
pub const EV_UNFOCUS: u32 = 7;
/// Mouse wheel: `key` is notches as i32, positive = scroll down; x, y = pointer.
pub const EV_SCROLL: u32 = 8;
pub const EV_WAKE: u32 = 9;
/// x, y = the client size the user asked for; take it with SYS_WIN_RESIZE.
pub const EV_RESIZE: u32 = 10;

pub const KEY_UP: u32 = 0x100;
pub const KEY_DOWN: u32 = 0x101;
pub const KEY_LEFT: u32 = 0x102;
pub const KEY_RIGHT: u32 = 0x103;
pub const KEY_HOME: u32 = 0x104;
pub const KEY_END: u32 = 0x105;
pub const KEY_PGUP: u32 = 0x106;
pub const KEY_PGDN: u32 = 0x107;
pub const KEY_DELETE: u32 = 0x108;

pub const CPU_FEAT_NX: u32 = 1 << 0;
pub const CPU_FEAT_SMEP: u32 = 1 << 1;
pub const CPU_FEAT_SMAP: u32 = 1 << 2;
pub const CPU_FEAT_UMIP: u32 = 1 << 3;
pub const CPU_FEAT_RDRAND: u32 = 1 << 4;

pub const MOD_SHIFT: u32 = 1;
pub const MOD_CTRL: u32 = 2;
pub const MOD_ALT: u32 = 4;
pub const MOD_SUPER: u32 = 8;

#[repr(C)]
#[derive(Clone, Copy)]
pub struct win_req {
    pub width: u32,
    pub height: u32,
    pub title: [u8; WIN_TITLE_MAX],
}

/// Pixels are 0x00RRGGBB, row-major, `pitch` pixels per row.
#[repr(C)]
#[derive(Clone, Copy, Default, Debug)]
pub struct win_info {
    pub pixels: u64,
    pub width: u32,
    pub height: u32,
    pub pitch: u32,
    pub pad: u32,
}

#[repr(C)]
#[derive(Clone, Copy, Default, Debug)]
pub struct event {
    pub type_: u32,
    pub window: u32,
    pub x: i32,
    pub y: i32,
    pub key: u32,
    pub mods: u32,
    pub buttons: u32,
    pub pad: u32,
}

#[repr(C)]
#[derive(Clone, Copy, Default, Debug)]
pub struct time_info {
    pub uptime_ms: u64,
    pub year: u16,
    pub month: u8,
    pub day: u8,
    pub hour: u8,
    pub minute: u8,
    pub second: u8,
    pub pad: u8,
}

#[repr(C)]
#[derive(Clone, Copy, Default, Debug)]
pub struct file_stat {
    pub size: u64,
    pub mtime: u64,
    pub type_: u32,
    pub label: u32,
}

#[repr(C)]
#[derive(Clone, Copy)]
pub struct dir_entry {
    pub name: [u8; NAME_MAX + 2],
    pub type_: u32,
    pub label: u32,
    pub size: u64,
}

#[repr(C)]
#[derive(Clone, Copy)]
pub struct pick_req {
    pub mode: u32,
    pub pad: u32,
    pub suggested: [u8; NAME_MAX + 2],
}

#[repr(C)]
#[derive(Clone, Copy)]
pub struct pick_result {
    pub name: [u8; NAME_MAX + 2],
    pub label: u32,
    pub pad: u32,
}

#[repr(C)]
#[derive(Clone, Copy, Default, Debug)]
pub struct fs_info {
    pub kind: u32,
    pub block_size: u32,
    pub total_blocks: u64,
    pub free_blocks: u64,
    pub total_inodes: u32,
    pub free_inodes: u32,
    pub taint: u32,
    pub pad: u32,
    pub device: [u8; 32],
}

/// The screen, and the largest client area that fits between the top bar and the dock.
#[repr(C)]
#[derive(Clone, Copy, Default, Debug)]
pub struct display_info {
    pub screen_w: u32,
    pub screen_h: u32,
    pub max_client_w: u32,
    pub max_client_h: u32,
}

#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct sys_info {
    pub mem_total_kib: u64,
    pub mem_free_kib: u64,
    pub uptime_ms: u64,
    pub nproc: u32,
    pub screen_w: u32,
    pub screen_h: u32,
    pub cpu_features: u32,
    pub cpu_vendor: [u8; 16],
    pub cpu_brand: [u8; 48],
}

impl Default for sys_info {
    fn default() -> Self {
        Self {
            mem_total_kib: 0,
            mem_free_kib: 0,
            uptime_ms: 0,
            nproc: 0,
            screen_w: 0,
            screen_h: 0,
            cpu_features: 0,
            cpu_vendor: [0; 16],
            cpu_brand: [0; 48],
        }
    }
}

#[repr(C)]
#[derive(Clone, Copy, Default, Debug)]
pub struct net_info {
    /// 1 when a network card is up.
    pub link: u32,
    /// 1 once DHCP gave us an address.
    pub configured: u32,
    pub mac: [u8; 6],
    pub pad: [u8; 2],
    /// IPv4, network byte order.
    pub ip: u32,
    pub netmask: u32,
    pub gateway: u32,
    pub dns: u32,
    pub rx_packets: u64,
    pub tx_packets: u64,
    /// Blocked by the firewall or malformed.
    pub dropped: u64,
    pub connections: u32,
    pub pad2: u32,
}

#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct fw_rule {
    pub app: [u8; 16],
    pub host: [u8; NET_HOST_MAX],
    pub port: u16,
    pub action: u16,
    pub hits: u32,
}

impl Default for fw_rule {
    fn default() -> Self {
        Self { app: [0; 16], host: [0; NET_HOST_MAX], port: 0, action: 0, hits: 0 }
    }
}

pub const FW_ALLOW: u16 = 1;
pub const FW_DENY: u16 = 2;
pub const FW_COUNT: u32 = 0;
pub const FW_GET: u32 = 1;
pub const FW_ADD: u32 = 2;
pub const FW_REMOVE: u32 = 3;

pub const NET_MODE_DHCP: u32 = 0;
pub const NET_MODE_STATIC: u32 = 1;
pub const NETCFG_GET: u32 = 0;
pub const NETCFG_SET: u32 = 1;

#[repr(C)]
#[derive(Clone, Copy, Default, Debug)]
pub struct net_config {
    pub mode: u32,
    pub ip: u32,
    pub netmask: u32,
    pub gateway: u32,
    pub dns: u32,
}

#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct wifi_info {
    pub present: u32,
    pub enabled: u32,
    pub connected: u32,
    pub signal: i32,
    pub ssid: [u8; 32],
    pub detail: [u8; 64],
}

impl Default for wifi_info {
    fn default() -> Self {
        Self { present: 0, enabled: 0, connected: 0, signal: 0, ssid: [0; 32], detail: [0; 64] }
    }
}

pub const APP_NAME_MAX: usize = 16;
pub const APP_ICON_MAX: usize = 256;
/// Largest package the kernel installs (APP_MAX_BYTES in kernel/appstore.h).
pub const APP_MAX_BYTES: usize = 128 << 20;
pub const APP_TITLE_MAX: usize = 32;
pub const APP_VERSION_MAX: usize = 32;
pub const APP_SOURCE_MAX: usize = 96;

#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct app_info {
    pub name: [u8; APP_NAME_MAX],
    pub title: [u8; APP_TITLE_MAX],
    pub version: [u8; APP_VERSION_MAX],
    pub source: [u8; APP_SOURCE_MAX],
    pub caps: u32,
    pub trust: u32,
    pub abi: u32,
    pub system: u32,
    pub installed: u32,
    pub running: u32,
    pub update_pending: u32,
    pub icon_color: u32,
    pub mem_quota_mib: u32,
    pub size: u64,
}

impl Default for app_info {
    fn default() -> Self {
        Self {
            name: [0; APP_NAME_MAX],
            title: [0; APP_TITLE_MAX],
            version: [0; APP_VERSION_MAX],
            source: [0; APP_SOURCE_MAX],
            caps: 0,
            trust: 0,
            abi: 0,
            system: 0,
            installed: 0,
            running: 0,
            update_pending: 0,
            icon_color: 0,
            mem_quota_mib: 0,
            size: 0,
        }
    }
}

pub const APP_INSTALL_REPLACE: u32 = 1;
pub const APP_INSTALL_SIGNED: u32 = 2;

pub const SETTINGS_KEY_MAX: usize = 32;
pub const SETTINGS_VALUE_MAX: usize = 128;
pub const OS_VERSION_MAX: usize = 16;
pub const OS_BUILD_MAX: usize = 32;

#[repr(C)]
#[derive(Clone, Copy, Default, Debug)]
pub struct os_info {
    pub version: [u8; OS_VERSION_MAX],
    pub build: [u8; OS_BUILD_MAX],
    pub abi_version: u32,
    pub pad: u32,
    pub uptime_ms: u64,
}

pub const BOOT_FLAG_NETBOOT: u32 = 1 << 0;
pub const BOOT_FLAG_INSTALLED: u32 = 1 << 1;
pub const BOOT_NO_SLOT: u32 = 0xFFFF_FFFF;

pub const DISK_NAME_MAX: usize = 16;
pub const DISK_MODEL_MAX: usize = 41;
pub const SYSTEM_PASSPHRASE_MAX: usize = 128;

#[repr(C)]
#[derive(Clone, Copy)]
pub struct disk_info {
    pub name: [u8; DISK_NAME_MAX],
    pub model: [u8; DISK_MODEL_MAX],
    pub pad: [u8; 3],
    pub sectors: u64,
    pub installed: u32,
    pub has_gpt: u32,
    pub target_ok: u32,
    pub pad2: u32,
}

impl Default for disk_info {
    fn default() -> Self {
        Self {
            name: [0; DISK_NAME_MAX],
            model: [0; DISK_MODEL_MAX],
            pad: [0; 3],
            sectors: 0,
            installed: 0,
            has_gpt: 0,
            target_ok: 0,
            pad2: 0,
        }
    }
}

#[repr(C)]
#[derive(Clone, Copy, Default)]
pub struct update_info {
    pub boot_flags: u32,
    pub boot_slot: u32,
    pub installed: u32,
    pub slots: u32,
    pub running_slot: u32,
    pub active_slot: u32,
    pub good_slot: u32,
    pub pending_slot: u32,
    pub attempts: u32,
    pub pad: u32,
    pub slot_bytes: u64,
    pub running_version: [u8; 32],
    pub running_build: [u8; 32],
    pub other_version: [u8; 32],
    pub other_build: [u8; 32],
}

#[repr(C)]
#[derive(Clone, Copy)]
pub struct system_install_req {
    pub boot_image: u64,
    pub boot_len: u64,
    pub system_image: u64,
    pub system_len: u64,
    pub disk: u32,
    pub flags: u32,
    pub passphrase: [u8; SYSTEM_PASSPHRASE_MAX],
}

impl Default for system_install_req {
    fn default() -> Self {
        Self {
            boot_image: 0,
            boot_len: 0,
            system_image: 0,
            system_len: 0,
            disk: 0,
            flags: 0,
            passphrase: [0; SYSTEM_PASSPHRASE_MAX],
        }
    }
}

// Sizes as the C compiler lays them out (x86_64 SysV).
const _: () = assert!(core::mem::size_of::<win_req>() == 48);
const _: () = assert!(core::mem::size_of::<win_info>() == 24);
const _: () = assert!(core::mem::size_of::<event>() == 32);
const _: () = assert!(core::mem::size_of::<time_info>() == 16);
const _: () = assert!(core::mem::size_of::<file_stat>() == 24);
const _: () = assert!(core::mem::size_of::<dir_entry>() == 80);
const _: () = assert!(core::mem::size_of::<pick_req>() == 68);
const _: () = assert!(core::mem::size_of::<pick_result>() == 68);
const _: () = assert!(core::mem::size_of::<fs_info>() == 72);
const _: () = assert!(core::mem::size_of::<display_info>() == 16);
const _: () = assert!(core::mem::size_of::<sys_info>() == 104);
const _: () = assert!(core::mem::size_of::<net_info>() == 64);
const _: () = assert!(core::mem::offset_of!(net_info, ip) == 16);
const _: () = assert!(core::mem::offset_of!(net_info, rx_packets) == 32);
const _: () = assert!(core::mem::size_of::<fw_rule>() == 88);
const _: () = assert!(core::mem::size_of::<net_config>() == 20);
const _: () = assert!(core::mem::size_of::<wifi_info>() == 112);
const _: () = assert!(core::mem::size_of::<app_info>() == 224);
const _: () = assert!(core::mem::size_of::<os_info>() == 64);
const _: () = assert!(core::mem::size_of::<disk_info>() == 88);
const _: () = assert!(core::mem::size_of::<update_info>() == 176);
const _: () = assert!(core::mem::size_of::<system_install_req>() == 168);

/// Copies `s` into a fixed C string field, truncated on a UTF-8 boundary and
/// always NUL-terminated.
pub fn copy_cstr(dst: &mut [u8], s: &str) {
    let mut n = s.len().min(dst.len().saturating_sub(1));
    while n > 0 && !s.is_char_boundary(n) {
        n -= 1;
    }
    dst[..n].copy_from_slice(&s.as_bytes()[..n]);
    for b in &mut dst[n..] {
        *b = 0;
    }
}

/// The text of a NUL-terminated C string field (lossy: stops at bad UTF-8).
pub fn cstr(src: &[u8]) -> &str {
    let n = src.iter().position(|&b| b == 0).unwrap_or(src.len());
    match core::str::from_utf8(&src[..n]) {
        Ok(s) => s,
        Err(e) => core::str::from_utf8(&src[..e.valid_up_to()]).unwrap_or(""),
    }
}
