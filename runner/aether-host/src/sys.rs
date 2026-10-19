//! sys.rs - raw æther system calls, reached from a Linux-ABI process.
//!
//! Native call N is `syscall` with rax = 0x1000 + N and arguments in rdi, rsi,
//! rdx, r10, r8. A negative result is -E_*. These are thin and `unsafe`
//! where the kernel reads or writes through a pointer we pass; the typed
//! wrappers below check lengths and build the structs.

use crate::abi::*;
use crate::Error;

/// Issues native call `n`. Only meaningful under æther: on Linux, numbers
/// above 0x1000 fail with -ENOSYS.
///
/// # Safety
/// Pointer arguments must be valid for what call `n` reads or writes.
#[cfg(target_arch = "x86_64")]
#[inline]
pub unsafe fn syscall5(n: u64, a: u64, b: u64, c: u64, d: u64, e: u64) -> i64 {
    let r: i64;
    core::arch::asm!(
        "syscall",
        inlateout("rax") NATIVE_BASE + n => r,
        in("rdi") a, in("rsi") b, in("rdx") c, in("r10") d, in("r8") e,
        lateout("rcx") _, lateout("r11") _,
        options(nostack),
    );
    r
}

#[cfg(not(target_arch = "x86_64"))]
pub unsafe fn syscall5(_n: u64, _a: u64, _b: u64, _c: u64, _d: u64, _e: u64) -> i64 {
    -(E_INVAL as i64)
}

fn check(r: i64) -> Result<u64, Error> {
    if r < 0 {
        Err(Error(-r as i32))
    } else {
        Ok(r as u64)
    }
}

pub fn exit(code: i32) -> ! {
    unsafe { syscall5(SYS_EXIT, code as u64, 0, 0, 0, 0) };
    loop {
        core::hint::spin_loop();
    }
}

pub fn log(msg: &str) -> Result<(), Error> {
    check(unsafe { syscall5(SYS_LOG, msg.as_ptr() as u64, msg.len() as u64, 0, 0, 0) }).map(|_| ())
}

/// Creates a window; returns its handle and where its pixels are.
pub fn win_create(width: u32, height: u32, title: &str) -> Result<(u32, win_info), Error> {
    let mut req = win_req { width, height, title: [0; WIN_TITLE_MAX] };
    copy_cstr(&mut req.title, title);
    let mut info = win_info::default();
    let h = check(unsafe {
        syscall5(SYS_WIN_CREATE, &req as *const _ as u64, &mut info as *mut _ as u64, 0, 0, 0)
    })?;
    Ok((h as u32, info))
}

pub fn win_present(handle: u32) -> Result<(), Error> {
    check(unsafe { syscall5(SYS_WIN_PRESENT, handle as u64, 0, 0, 0, 0) }).map(|_| ())
}

/// Opts the window in (or out) of being zoomed and resized by the user, and
/// says the smallest client size the app can lay out.
pub fn win_flags(handle: u32, flags: u32, min_w: u32, min_h: u32) -> Result<(), Error> {
    check(unsafe { syscall5(SYS_WIN_FLAGS, handle as u64, flags as u64, min_w as u64, min_h as u64, 0) }).map(|_| ())
}

/// Takes the size the window manager proposed with EV_RESIZE. The pixel
/// buffer is replaced (at the same address): use the returned info from now on.
pub fn win_resize(handle: u32) -> Result<win_info, Error> {
    let mut info = win_info::default();
    check(unsafe { syscall5(SYS_WIN_RESIZE, handle as u64, &mut info as *mut _ as u64, 0, 0, 0) })?;
    Ok(info)
}

pub fn win_title(handle: u32, title: &str) -> Result<(), Error> {
    let n = title.len().min(WIN_TITLE_MAX - 1);
    let mut n2 = n;
    while n2 > 0 && !title.is_char_boundary(n2) {
        n2 -= 1;
    }
    check(unsafe { syscall5(SYS_WIN_TITLE, handle as u64, title.as_ptr() as u64, n2 as u64, 0, 0) })
        .map(|_| ())
}

/// Waits for one event. `timeout_ms` None waits forever. Ok(None) on timeout.
pub fn event_wait(timeout_ms: Option<u32>) -> Result<Option<event>, Error> {
    let mut ev = event::default();
    let t = match timeout_ms {
        Some(ms) => ms as u64,
        None => u64::MAX, // -1
    };
    let r = check(unsafe { syscall5(SYS_EVENT_WAIT, &mut ev as *mut _ as u64, t, 0, 0, 0) })?;
    Ok(if r == 0 { None } else { Some(ev) })
}

pub fn time() -> Result<time_info, Error> {
    let mut t = time_info::default();
    check(unsafe { syscall5(SYS_TIME, &mut t as *mut _ as u64, 0, 0, 0, 0) })?;
    Ok(t)
}

/// Asks the user for a file. Returns an open handle and the chosen name.
pub fn pick_file(mode: u32, suggested: &str) -> Result<(u32, pick_result), Error> {
    let mut req = pick_req { mode, pad: 0, suggested: [0; NAME_MAX + 2] };
    copy_cstr(&mut req.suggested, suggested);
    let mut out = pick_result { name: [0; NAME_MAX + 2], label: 0, pad: 0 };
    let h = check(unsafe {
        syscall5(SYS_PICK_FILE, &req as *const _ as u64, &mut out as *mut _ as u64, 0, 0, 0)
    })?;
    Ok((h as u32, out))
}

pub fn read(handle: u32, buf: &mut [u8]) -> Result<usize, Error> {
    check(unsafe { syscall5(SYS_READ, handle as u64, buf.as_mut_ptr() as u64, buf.len() as u64, 0, 0) })
        .map(|n| n as usize)
}

pub fn write(handle: u32, buf: &[u8]) -> Result<usize, Error> {
    check(unsafe { syscall5(SYS_WRITE, handle as u64, buf.as_ptr() as u64, buf.len() as u64, 0, 0) })
        .map(|n| n as usize)
}

pub fn seek(handle: u32, offset: i64, whence: u32) -> Result<u64, Error> {
    check(unsafe { syscall5(SYS_SEEK, handle as u64, offset as u64, whence as u64, 0, 0) })
}

/// fstat: SYS_STAT with path 0 and the handle as the length.
pub fn fstat(handle: u32) -> Result<file_stat, Error> {
    let mut st = file_stat::default();
    check(unsafe { syscall5(SYS_STAT, 0, handle as u64, &mut st as *mut _ as u64, 0, 0) })?;
    Ok(st)
}

pub fn close(handle: u32) -> Result<(), Error> {
    check(unsafe { syscall5(SYS_CLOSE, handle as u64, 0, 0, 0, 0) }).map(|_| ())
}

pub fn fs_info() -> Result<fs_info, Error> {
    let mut fi = fs_info::default();
    check(unsafe { syscall5(SYS_FS_INFO, &mut fi as *mut _ as u64, 0, 0, 0, 0) })?;
    Ok(fi)
}

pub fn display_info() -> Result<display_info, Error> {
    let mut d = display_info::default();
    check(unsafe { syscall5(SYS_DISPLAY_INFO, &mut d as *mut _ as u64, 0, 0, 0, 0) })?;
    Ok(d)
}

/// Wakes this process's SYS_EVENT_WAIT from another thread (queues one EV_WAKE).
pub fn event_wake() -> Result<(), Error> {
    check(unsafe { syscall5(SYS_EVENT_WAKE, 0, 0, 0, 0, 0) }).map(|_| ())
}

/// Opens an outgoing TCP connection. The kernel resolves `host` and the
/// firewall decides before any packet is sent. Returns a network handle.
pub fn net_connect(host: &str, port: u16, timeout_ms: u32) -> Result<u32, Error> {
    if host.is_empty() || host.len() >= NET_HOST_MAX {
        return Err(Error(E_INVAL));
    }
    let h = check(unsafe {
        syscall5(SYS_NET_CONNECT, host.as_ptr() as u64, host.len() as u64, port as u64, timeout_ms as u64, 0)
    })?;
    Ok(h as u32)
}

/// Sends from `buf`; returns how many bytes the kernel accepted (it blocks
/// while the send window is full).
pub fn net_send(handle: u32, buf: &[u8]) -> Result<usize, Error> {
    check(unsafe { syscall5(SYS_NET_SEND, handle as u64, buf.as_ptr() as u64, buf.len() as u64, 0, 0) })
        .map(|n| n as usize)
}

/// Receives into `buf`. `timeout_ms` None waits forever. Ok(0): the peer
/// closed; Err(E_TIMEOUT) when nothing arrived in time.
pub fn net_recv(handle: u32, buf: &mut [u8], timeout_ms: Option<u32>) -> Result<usize, Error> {
    let t = match timeout_ms {
        Some(ms) => ms as u64,
        None => u64::MAX, // -1
    };
    check(unsafe {
        syscall5(SYS_NET_RECV, handle as u64, buf.as_mut_ptr() as u64, buf.len() as u64, t, 0)
    })
    .map(|n| n as usize)
}

pub fn net_close(handle: u32) -> Result<(), Error> {
    check(unsafe { syscall5(SYS_NET_CLOSE, handle as u64, 0, 0, 0, 0) }).map(|_| ())
}

pub fn net_info() -> Result<net_info, Error> {
    let mut ni = net_info::default();
    check(unsafe { syscall5(SYS_NET_INFO, &mut ni as *mut _ as u64, 0, 0, 0, 0) })?;
    Ok(ni)
}

/// Reads (`NETCFG_GET`) or changes (`NETCFG_SET`) the network configuration.
pub fn net_config(op: u32, cfg: &net_config) -> Result<net_config, Error> {
    let mut out = net_config::default();
    check(unsafe {
        syscall5(SYS_NET_CONFIG, op as u64, cfg as *const _ as u64, &mut out as *mut _ as u64, 0, 0)
    })?;
    Ok(out)
}

pub fn firewall(op: u32, rule: Option<&fw_rule>, index: u32) -> Result<fw_rule, Error> {
    let mut out = fw_rule::default();
    let r = match rule {
        Some(r) => r as *const _ as u64,
        None => &mut out as *mut _ as u64,
    };
    check(unsafe { syscall5(SYS_FIREWALL, op as u64, r, index as u64, 0, 0) })?;
    Ok(out)
}

pub fn firewall_count() -> Result<u32, Error> {
    check(unsafe { syscall5(SYS_FIREWALL, FW_COUNT as u64, 0, 0, 0, 0) }).map(|n| n as u32)
}

pub fn firewall_get(index: u32) -> Result<fw_rule, Error> {
    let mut out = fw_rule::default();
    check(unsafe { syscall5(SYS_FIREWALL, FW_GET as u64, &mut out as *mut _ as u64, index as u64, 0, 0) })?;
    Ok(out)
}

pub fn firewall_add(rule: &fw_rule) -> Result<(), Error> {
    check(unsafe { syscall5(SYS_FIREWALL, FW_ADD as u64, rule as *const _ as u64, 0, 0, 0) }).map(|_| ())
}

pub fn firewall_remove(index: u32) -> Result<(), Error> {
    check(unsafe { syscall5(SYS_FIREWALL, FW_REMOVE as u64, 0, index as u64, 0, 0) }).map(|_| ())
}

/// Opens a file or folder by path (needs CAP_FILES or CAP_STORAGE).
pub fn open(path: &str, flags: u32) -> Result<u32, Error> {
    if path.is_empty() || path.len() >= PATH_MAX {
        return Err(Error(E_INVAL));
    }
    check(unsafe {
        syscall5(SYS_OPEN, path.as_ptr() as u64, path.len() as u64, flags as u64, 0, 0)
    })
    .map(|h| h as u32)
}

/// `stat` by path; the path is relative to the app's private folder unless it
/// starts with '/', in which case it needs CAP_FILES.
pub fn stat_path(path: &str) -> Result<file_stat, Error> {
    if path.is_empty() || path.len() >= PATH_MAX {
        return Err(Error(E_INVAL));
    }
    let mut st = file_stat::default();
    check(unsafe {
        syscall5(SYS_STAT, path.as_ptr() as u64, path.len() as u64, &mut st as *mut _ as u64, 0, 0)
    })?;
    Ok(st)
}

/// One entry of an open folder; Ok(None) past the end.
pub fn readdir(handle: u32, index: u32) -> Result<Option<dir_entry>, Error> {
    let mut de = dir_entry { name: [0; NAME_MAX + 2], type_: 0, label: 0, size: 0 };
    let r = check(unsafe {
        syscall5(SYS_READDIR, handle as u64, index as u64, &mut de as *mut _ as u64, 0, 0)
    })?;
    Ok(if r == 0 { None } else { Some(de) })
}

pub fn mkdir(path: &str) -> Result<(), Error> {
    if path.is_empty() || path.len() >= PATH_MAX {
        return Err(Error(E_INVAL));
    }
    check(unsafe { syscall5(SYS_MKDIR, path.as_ptr() as u64, path.len() as u64, 0, 0, 0) }).map(|_| ())
}

pub fn remove(path: &str) -> Result<(), Error> {
    if path.is_empty() || path.len() >= PATH_MAX {
        return Err(Error(E_INVAL));
    }
    check(unsafe { syscall5(SYS_REMOVE, path.as_ptr() as u64, path.len() as u64, 0, 0, 0) }).map(|_| ())
}

pub fn rename(from: &str, to: &str) -> Result<(), Error> {
    if from.is_empty() || to.is_empty() || from.len() >= PATH_MAX || to.len() >= PATH_MAX {
        return Err(Error(E_INVAL));
    }
    check(unsafe {
        syscall5(SYS_RENAME, from.as_ptr() as u64, from.len() as u64, to.as_ptr() as u64, to.len() as u64, 0)
    })
    .map(|_| ())
}

pub fn set_label(path: &str, label: u32) -> Result<(), Error> {
    if path.is_empty() || path.len() >= PATH_MAX {
        return Err(Error(E_INVAL));
    }
    check(unsafe {
        syscall5(SYS_SET_LABEL, path.as_ptr() as u64, path.len() as u64, label as u64, 0, 0)
    })
    .map(|_| ())
}

pub fn mem_map(len: usize, prot: u32) -> Result<u64, Error> {
    check(unsafe { syscall5(SYS_MEM_MAP, len as u64, prot as u64, 0, 0, 0) })
}

pub fn mem_unmap(addr: u64, len: usize) -> Result<(), Error> {
    check(unsafe { syscall5(SYS_MEM_UNMAP, addr, len as u64, 0, 0, 0) }).map(|_| ())
}

pub fn mem_protect(addr: u64, len: usize, prot: u32) -> Result<(), Error> {
    check(unsafe { syscall5(SYS_MEM_PROTECT, addr, len as u64, prot as u64, 0, 0) }).map(|_| ())
}

pub fn dlp_log(buf: &mut [u8]) -> Result<usize, Error> {
    check(unsafe { syscall5(SYS_DLP_LOG, buf.as_mut_ptr() as u64, buf.len() as u64, 0, 0, 0) })
        .map(|n| n as usize)
}

pub fn sysinfo() -> Result<sys_info, Error> {
    let mut si = sys_info::default();
    check(unsafe { syscall5(SYS_SYSINFO, &mut si as *mut _ as u64, 0, 0, 0, 0) })?;
    Ok(si)
}

pub fn spawn(name: &str, caps: u32, arg: u64) -> Result<i32, Error> {
    if name.is_empty() || name.len() >= 16 {
        return Err(Error(E_INVAL));
    }
    check(unsafe {
        syscall5(SYS_SPAWN, name.as_ptr() as u64, name.len() as u64, caps as u64, arg, 0)
    })
    .map(|pid| pid as i32)
}

pub fn wait(pid: i32, timeout_ms: i64) -> Result<i32, Error> {
    check(unsafe { syscall5(SYS_WAIT, pid as u64, timeout_ms as u64, 0, 0, 0) }).map(|s| s as i32)
}

pub fn app_list(out: &mut [app_info]) -> Result<usize, Error> {
    check(unsafe {
        syscall5(SYS_APP_LIST, out.as_mut_ptr() as u64, out.len() as u64, 0, 0, 0)
    })
    .map(|n| n as usize)
}

pub fn app_info(name: &str) -> Result<app_info, Error> {
    if name.is_empty() || name.len() >= APP_NAME_MAX {
        return Err(Error(E_INVAL));
    }
    let mut info = app_info::default();
    check(unsafe {
        syscall5(SYS_APP_INFO, name.as_ptr() as u64, name.len() as u64, &mut info as *mut _ as u64, 0, 0)
    })?;
    Ok(info)
}

/// The app's package icon: `out` receives RGBA8888; returns (width, height).
/// `Ok(None)` means the app has no icon. `out` should be at least
/// `APP_ICON_MAX * APP_ICON_MAX * 4` bytes for any future icon.
pub fn app_icon(name: &str, out: &mut [u8]) -> Result<Option<(u32, u32)>, Error> {
    if name.is_empty() || name.len() >= APP_NAME_MAX {
        return Err(Error(E_INVAL));
    }
    let r = check(unsafe {
        syscall5(SYS_APP_ICON, name.as_ptr() as u64, name.len() as u64, out.as_mut_ptr() as u64, out.len() as u64, 0)
    })?;
    if r == 0 {
        return Ok(None);
    }
    let w = (r >> 16) as u32;
    let h = (r & 0xFFFF) as u32;
    let size = w as usize * h as usize * 4;
    if w == 0 || h == 0 || w as usize > APP_ICON_MAX || h as usize > APP_ICON_MAX || size > out.len() {
        return Err(Error(E_IO));
    }
    Ok(Some((w, h)))
}

pub fn app_install(package: &[u8], replace: bool) -> Result<(), Error> {
    let flags = if replace { APP_INSTALL_REPLACE } else { 0 };
    check(unsafe {
        syscall5(SYS_APP_INSTALL, package.as_ptr() as u64, package.len() as u64, flags as u64, 0, 0)
    })
    .map(|_| ())
}

/// Install a package downloaded from a release. The kernel must verify its
/// publisher signature; a local file uses `app_install` instead.
pub fn app_install_signed(package: &[u8], replace: bool) -> Result<(), Error> {
    let flags = APP_INSTALL_SIGNED | if replace { APP_INSTALL_REPLACE } else { 0 };
    check(unsafe {
        syscall5(SYS_APP_INSTALL, package.as_ptr() as u64, package.len() as u64, flags as u64, 0, 0)
    })
    .map(|_| ())
}

pub fn app_remove(name: &str) -> Result<(), Error> {
    if name.is_empty() || name.len() >= APP_NAME_MAX {
        return Err(Error(E_INVAL));
    }
    check(unsafe { syscall5(SYS_APP_REMOVE, name.as_ptr() as u64, name.len() as u64, 0, 0, 0) }).map(|_| ())
}

/// Changes the mounted volume's passphrase. The kernel verifies the old one
/// and re-wraps the volume key; it never stores either passphrase.
/// Post a notification. The kernel records this app's identity, rate-limits
/// bursts, and draws the history (Super+N).
pub fn notify(title: &str, msg: &str) -> Result<(), Error> {
    check(unsafe {
        syscall5(
            SYS_NOTIFY,
            title.as_ptr() as u64,
            title.len() as u64,
            msg.as_ptr() as u64,
            msg.len() as u64,
            0,
        )
    })
    .map(|_| ())
}

pub fn change_passphrase(old: &str, new: &str) -> Result<(), Error> {
    if old.is_empty() || new.len() < 8 {
        return Err(Error(E_INVAL));
    }
    check(unsafe {
        syscall5(
            SYS_CHANGE_PASSPHRASE,
            old.as_ptr() as u64,
            old.len() as u64,
            new.as_ptr() as u64,
            new.len() as u64,
            0,
        )
    })
    .map(|_| ())
}

/// Locks the desktop (the session lock, not the boot-time disk unlock).
pub fn lock_screen() -> Result<(), Error> {
    check(unsafe { syscall5(SYS_LOCK, 0, 0, 0, 0, 0) }).map(|_| ())
}

pub fn settings_get(key: &str, out: &mut [u8]) -> Result<usize, Error> {
    if key.is_empty() || key.len() >= SETTINGS_KEY_MAX {
        return Err(Error(E_INVAL));
    }
    check(unsafe {
        syscall5(SYS_SETTINGS_GET, key.as_ptr() as u64, key.len() as u64, out.as_mut_ptr() as u64, out.len() as u64, 0)
    })
    .map(|n| n as usize)
}

pub fn settings_set(key: &str, value: &str) -> Result<(), Error> {
    if key.is_empty() || key.len() >= SETTINGS_KEY_MAX || value.len() >= SETTINGS_VALUE_MAX {
        return Err(Error(E_INVAL));
    }
    check(unsafe {
        syscall5(SYS_SETTINGS_SET, key.as_ptr() as u64, key.len() as u64, value.as_ptr() as u64, value.len() as u64, 0)
    })
    .map(|_| ())
}

pub fn os_info() -> Result<os_info, Error> {
    let mut oi = os_info::default();
    check(unsafe { syscall5(SYS_OS_INFO, &mut oi as *mut _ as u64, 0, 0, 0, 0) })?;
    Ok(oi)
}

pub fn wifi_info() -> Result<wifi_info, Error> {
    let mut wi = wifi_info::default();
    check(unsafe { syscall5(SYS_WIFI_INFO, &mut wi as *mut _ as u64, 0, 0, 0, 0) })?;
    Ok(wi)
}

// ---- Installation and A/B updates (0.8) --------------------------------

/// Whole disks the installer may target.
pub fn disk_list(out: &mut [disk_info]) -> Result<usize, Error> {
    check(unsafe {
        syscall5(SYS_DISK_LIST, out.as_mut_ptr() as u64, out.len() as u64, 0, 0, 0)
    })
    .map(|n| n as usize)
}

/// Install a fresh system. `boot` is the FAT ESP image and `system` is the
/// sysimage; both are checked again by the kernel before anything is written.
pub fn system_install(disk: u32, boot: &[u8], system: &[u8], passphrase: &str) -> Result<(), Error> {
    let mut req = system_install_req::default();
    req.boot_image = boot.as_ptr() as u64;
    req.boot_len = boot.len() as u64;
    req.system_image = system.as_ptr() as u64;
    req.system_len = system.len() as u64;
    req.disk = disk;
    copy_cstr(&mut req.passphrase, passphrase);
    check(unsafe { syscall5(SYS_SYSTEM_INSTALL, &req as *const _ as u64, 0, 0, 0, 0) }).map(|_| ())
}

pub fn update_info() -> Result<update_info, Error> {
    let mut out = update_info::default();
    check(unsafe { syscall5(SYS_UPDATE_INFO, &mut out as *mut _ as u64, 0, 0, 0, 0) })?;
    Ok(out)
}

/// Write a sysimage to the inactive slot. It becomes the next boot.
pub fn update_install(image: &[u8]) -> Result<(), Error> {
    check(unsafe {
        syscall5(SYS_UPDATE_INSTALL, image.as_ptr() as u64, image.len() as u64, 0, 0, 0)
    })
    .map(|_| ())
}

pub fn reboot() -> ! {
    unsafe { syscall5(SYS_REBOOT, 0, 0, 0, 0, 0) };
    loop {
        core::hint::spin_loop();
    }
}
