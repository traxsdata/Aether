//! aether-nvme: a small, polling NVMe driver written in safe Rust.
//!
//! The driver is linked into the kernel and speaks the same `struct blkdev`
//! interface as the C drivers. Memory safety rules used here:
//!
//! * all DMA memory comes from `pmm_alloc_contig`, is page-aligned, zeroed,
//!   owned by this driver and never handed to user space;
//! * queue depths, doorbell strides, command sizes, LBA ranges and completion
//!   statuses are checked before use;
//! * every hardware access goes through a small `unsafe` MMIO wrapper, and
//!   no `unsafe` block performs arithmetic on untrusted values;
//! * the driver polls bounded queues with a cycle deadline, so a broken
//!   device cannot spin the kernel forever;
//! * it never allocates, panics or formats log strings on the I/O path.
//!
//! The kernel is single-threaded while devices are initialised and block I/O
//! from different processes is serialised by the kernel's syscall path, so
//! one controller state is enough. The static is only mutated during init.

#![no_std]
#![forbid(unsafe_op_in_unsafe_fn)]

use core::ffi::c_void;
use core::ptr;
use core::sync::atomic::{compiler_fence, Ordering};

const HHDM_BASE: u64 = 0xFFFF_8000_0000_0000;
const PAGE: u64 = 4096;
const MMIO_SIZE: u64 = 0x4000;
const MMIO_WINDOW: u64 = 0xFFFF_FE00_0200_0000;
const ADMIN_QID: u16 = 0;
const IO_QID: u16 = 1;
const QDEPTH: u16 = 64;
const BOUNCE_PAGES: u32 = 64;
const BOUNCE_BYTES: u32 = BOUNCE_PAGES * 4096;
const TIMEOUT_MS: u64 = 15_000;
const BLK_EIO: i32 = -1;
const BLK_ETIMEDOUT: i32 = -2;
const BLK_ERANGE: i32 = -3;
const BLK_ENOMEM: i32 = -4;
const BLK_EINVAL: i32 = -5;


/* ---- C interfaces ------------------------------------------------------ */

#[repr(C)]
pub struct PciDev {
    bus: u8,
    dev: u8,
    fn_: u8,
    vendor: u16,
    device: u16,
    class: u8,
    subclass: u8,
    progif: u8,
}

#[repr(C)]
pub struct BlkDev {
    name: [u8; 16],
    model: [u8; 41],
    sectors: u64,
    read: Option<unsafe extern "C" fn(*mut BlkDev, u64, u32, *mut c_void) -> i32>,
    write: Option<unsafe extern "C" fn(*mut BlkDev, u64, u32, *const c_void) -> i32>,
    flush: Option<unsafe extern "C" fn(*mut BlkDev) -> i32>,
    priv_: *mut c_void,
}

const _: () = assert!(core::mem::size_of::<PciDev>() == 12);
const _: () = assert!(core::mem::size_of::<BlkDev>() == 104);

extern "C" {
    fn pci_find_class(class: u8, subclass: u8, progif: u8, idx: i32) -> *mut PciDev;
    fn aether_pci_bar_mem(dev: *mut PciDev, n: i32) -> u64;
    fn pci_enable_bus_master_mem(dev: *const PciDev);
    fn pmm_alloc_contig(pages: usize) -> u64;
    fn blk_register(dev: *mut BlkDev);
    fn aether_log(msg: *const u8);
    fn aether_pit_ms() -> u64;
    fn aether_map_mmio(virt: u64, phys: u64, len: u64) -> i32;
    fn aether_pci_disable(dev: *mut PciDev);
    fn aether_kernel_panic() -> !;
}

fn log(msg: &str) {
    let mut buf = [0u8; 160];
    let n = msg.len().min(buf.len() - 1);
    buf[..n].copy_from_slice(&msg.as_bytes()[..n]);
    unsafe { aether_log(buf.as_ptr()) };
}

fn phys_to_virt(pa: u64) -> *mut u8 {
    (pa.wrapping_add(HHDM_BASE)) as *mut u8
}

/* ---- Register definitions --------------------------------------------- */

const CAP: u64 = 0x00;
const VS: u64 = 0x08;
const CC: u64 = 0x14;
const CSTS: u64 = 0x1C;
const AQA: u64 = 0x24;
const ASQ: u64 = 0x28;
const ACQ: u64 = 0x30;

const CC_EN: u32 = 1 << 0;
const CC_CSS_NVM: u32 = 0 << 4;
const CC_IOSQES_64: u32 = 6 << 16;
const CC_IOCQES_16: u32 = 4 << 20;
const CSTS_RDY: u32 = 1 << 0;
const CSTS_CFS: u32 = 1 << 1;

#[inline]
unsafe fn raw_read32(mmio: *mut u8, off: u64) -> u32 {
    unsafe { ptr::read_volatile(mmio.add(off as usize) as *const u32) }
}
#[inline]
unsafe fn raw_write32(mmio: *mut u8, off: u64, v: u32) {
    unsafe { ptr::write_volatile(mmio.add(off as usize) as *mut u32, v) }
}
#[inline]
unsafe fn raw_read64(mmio: *mut u8, off: u64) -> u64 {
    unsafe { ptr::read_volatile(mmio.add(off as usize) as *const u64) }
}
#[inline]
unsafe fn raw_write64(mmio: *mut u8, off: u64, v: u64) {
    unsafe { ptr::write_volatile(mmio.add(off as usize) as *mut u64, v) }
}

#[inline]
fn pause() {
    core::arch::x86_64::_mm_pause()
}

fn deadline() -> u64 {
    let now = unsafe { aether_pit_ms() };
    now + TIMEOUT_MS
}

fn wait_until(deadline: u64, mut f: impl FnMut() -> bool) -> bool {
    loop {
        if f() {
            return true;
        }
        if unsafe { aether_pit_ms() } >= deadline {
            return false;
        }
        pause();
    }
}

/* ---- Data structures --------------------------------------------------- */

#[repr(C, align(64))]
#[derive(Clone, Copy)]
struct Command {
    cdw0: u32,
    nsid: u32,
    rsvd: [u32; 2],
    mptr: u64,
    prp1: u64,
    prp2: u64,
    cdw10: u32,
    cdw11: u32,
    cdw12: u32,
    cdw13: u32,
    cdw14: u32,
    cdw15: u32,
}
const _: () = assert!(core::mem::size_of::<Command>() == 64);

#[repr(C)]
#[derive(Clone, Copy, Default)]
struct Completion {
    dw0: u32,
    dw1: u32,
    dw2: u32,
    dw3: u32,
}
const _: () = assert!(core::mem::size_of::<Completion>() == 16);

struct Queue {
    sq: *mut Command,
    cq: *mut Completion,
    sq_tail: u16,
    cq_head: u16,
    phase: bool,
    cq_doorbell: u64,
    sq_doorbell: u64,
    depth: u16,
}

struct Controller {
    mmio: *mut u8,
    doorbell_stride: u64,
    admin: Queue,
    io: Queue,
    cid: u16,
    nsid: u32,
    sectors: u64,
    bounce_phys: u64,
    prp_phys: u64,
    prp: *mut u64,
    blk: BlkDev,
    name: [u8; 16],
    model: [u8; 41],
    failed: bool,
}

const _: () = assert!(core::mem::size_of::<Controller>() <= 4096);

static mut CONTROLLER: *mut Controller = ptr::null_mut();

/* ---- Queue operations -------------------------------------------------- */

fn ring_sq(ctrl: &mut Controller, q: QueueKind, cmd: Command) -> Result<(), i32> {
    let mmio = ctrl.mmio;
    let q = q.get(ctrl);
    if q.depth < 2 || q.sq_tail >= q.depth || q.sq.is_null() {
        return Err(BLK_EINVAL);
    }
    let doorbell = q.sq_doorbell;
    unsafe {
        ptr::write_volatile(q.sq.add(q.sq_tail as usize), cmd);
        compiler_fence(Ordering::SeqCst);
        let tail = (q.sq_tail + 1) % q.depth;
        q.sq_tail = tail;
        raw_write32(mmio, doorbell, tail as u32);
    }
    Ok(())
}

#[derive(Clone, Copy)]
enum QueueKind {
    Admin,
    Io,
}

impl QueueKind {
    fn get<'a>(self, c: &'a mut Controller) -> &'a mut Queue {
        match self {
            QueueKind::Admin => &mut c.admin,
            QueueKind::Io => &mut c.io,
        }
    }
}

fn execute(ctrl: &mut Controller, qk: QueueKind, cmd: Command) -> Result<u16, i32> {
    if ctrl.failed {
        return Err(BLK_EIO);
    }
    if unsafe { raw_read32(ctrl.mmio, CSTS) } & CSTS_CFS != 0 {
        ctrl.failed = true;
        return Err(BLK_EIO);
    }
    let cid = ctrl.cid;
    ctrl.cid = ctrl.cid.wrapping_add(1);
    let mut cmd = cmd;
    cmd.cdw0 = (cmd.cdw0 & 0xFFFF) | (cid as u32) << 16;
    ring_sq(ctrl, qk, cmd)?;
    let dbl = qk.get(ctrl).cq_doorbell;
    let dl = deadline();
    let mut spins = 0u64;
    loop {
        let mut completed: Option<(u16, u16)> = None;
        {
            let q = qk.get(ctrl);
            let c = unsafe { ptr::read_volatile(q.cq.add(q.cq_head as usize)) };
            let phase = (c.dw3 >> 16) & 1 != 0;
            if phase == q.phase {
                let got_cid = (c.dw3 & 0xFFFF) as u16;
                if got_cid != cid {
                    ctrl.failed = true;
                    unsafe { raw_write32(ctrl.mmio, CC, 0) };
                    return Err(BLK_EIO);
                }
                let status = ((c.dw3 >> 17) & 0x7FFF) as u16;
                let head = (q.cq_head + 1) % q.depth;
                if head == 0 {
                    q.phase = !q.phase;
                }
                q.cq_head = head;
                completed = Some((head, status));
            }
        }
        if let Some((head, status)) = completed {
            unsafe {
                compiler_fence(Ordering::SeqCst);
                raw_write32(ctrl.mmio, dbl, head as u32);
            }
            if status != 0 {
                log("NVMe completion error");
                return Err(BLK_EIO);
            }
            return Ok(cid);
        }
        if unsafe { aether_pit_ms() } >= dl {
            log("NVMe completion timeout");
            ctrl.failed = true;
            unsafe { raw_write32(ctrl.mmio, CC, 0) };
            return Err(BLK_ETIMEDOUT);
        }
        if spins % 4096 == 0 {
            pause();
        }
        spins = spins.wrapping_add(1);
    }
}

/* ---- Admin commands ---------------------------------------------------- */

fn identify(ctrl: &mut Controller, cns: u32, nsid: u32, out_phys: u64) -> Result<(), i32> {
    let cmd = Command {
        cdw0: 0x06,
        nsid,
        rsvd: [0; 2],
        mptr: 0,
        prp1: out_phys,
        prp2: 0,
        cdw10: cns,
        cdw11: 0,
        cdw12: 0,
        cdw13: 0,
        cdw14: 0,
        cdw15: 0,
    };
    execute(ctrl, QueueKind::Admin, cmd).map(|_| ())
}

fn create_io_queues(ctrl: &mut Controller, qid: u16, cq_phys: u64, sq_phys: u64, depth: u16) -> Result<(), i32> {
    let cq = Command {
        cdw0: 0x05, // create I/O completion queue
        nsid: 0,
        rsvd: [0; 2],
        mptr: 0,
        prp1: cq_phys,
        prp2: 0,
        cdw10: (((depth - 1) as u32) << 16) | qid as u32,
        cdw11: 1, // physically contiguous
        cdw12: 0,
        cdw13: 0,
        cdw14: 0,
        cdw15: 0,
    };
    execute(ctrl, QueueKind::Admin, cq)?;
    let sq = Command {
        cdw0: 0x01, // create I/O submission queue
        nsid: 0,
        rsvd: [0; 2],
        mptr: 0,
        prp1: sq_phys,
        prp2: 0,
        cdw10: (((depth - 1) as u32) << 16) | qid as u32,
        cdw11: (qid as u32) << 16 | 1, // CQID, physically contiguous
        cdw12: 0,
        cdw13: 0,
        cdw14: 0,
        cdw15: 0,
    };
    execute(ctrl, QueueKind::Admin, sq).map(|_| ())
}

/* ---- I/O --------------------------------------------------------------- */

fn submit_io(ctrl: &mut Controller, opcode: u32, lba: u64, sectors: u32) -> Result<(), i32> {
    if ctrl.failed {
        return Err(BLK_EIO);
    }
    if sectors == 0 || sectors > (BOUNCE_BYTES / 512) {
        return Err(BLK_EINVAL);
    }
    let pages = ((sectors as u64 * 512 + PAGE - 1) / PAGE) as usize;
    if pages > BOUNCE_PAGES as usize {
        return Err(BLK_ERANGE);
    }
    let prp2 = if pages == 2 {
        ctrl.bounce_phys + PAGE
    } else if pages > 2 {
        for i in 1..pages {
            unsafe { ptr::write_volatile(ctrl.prp.add(i - 1), ctrl.bounce_phys + i as u64 * PAGE) };
        }
        ctrl.prp_phys
    } else {
        0
    };
    let cmd = Command {
        cdw0: opcode,
        nsid: ctrl.nsid,
        rsvd: [0; 2],
        mptr: 0,
        prp1: ctrl.bounce_phys,
        prp2,
        cdw10: lba as u32,
        cdw11: (lba >> 32) as u32,
        cdw12: (sectors - 1) & 0xFFFF,
        cdw13: 0,
        cdw14: 0,
        cdw15: 0,
    };
    execute(ctrl, QueueKind::Io, cmd).map(|_| ())
}

unsafe extern "C" fn blk_read(d: *mut BlkDev, lba: u64, count: u32, buf: *mut c_void) -> i32 {
    if d.is_null() || buf.is_null() || count == 0 {
        return BLK_EINVAL;
    }
    let ctrl = unsafe { (*d).priv_ as *mut Controller };
    if ctrl.is_null() {
        return BLK_EINVAL;
    }
    let c = unsafe { &mut *ctrl };
    if c.failed {
        return BLK_EIO;
    }
    if lba >= c.sectors || count as u64 > c.sectors - lba {
        return BLK_ERANGE;
    }
    let mut done = 0u32;
    while done < count {
        let chunk = (count - done).min(BOUNCE_BYTES / 512);
        if let Err(e) = submit_io(c, 0x02, lba + done as u64, chunk) {
            return e;
        }
        let bytes = chunk as usize * 512;
        let dst = unsafe { (buf as *mut u8).add(done as usize * 512) };
        unsafe { ptr::copy_nonoverlapping(phys_to_virt(c.bounce_phys), dst, bytes) };
        done += chunk;
    }
    0
}

unsafe extern "C" fn blk_write(d: *mut BlkDev, lba: u64, count: u32, buf: *const c_void) -> i32 {
    if d.is_null() || buf.is_null() || count == 0 {
        return BLK_EINVAL;
    }
    let ctrl = unsafe { (*d).priv_ as *mut Controller };
    if ctrl.is_null() {
        return BLK_EINVAL;
    }
    let c = unsafe { &mut *ctrl };
    if c.failed {
        return BLK_EIO;
    }
    if lba >= c.sectors || count as u64 > c.sectors - lba {
        return BLK_ERANGE;
    }
    let mut done = 0u32;
    while done < count {
        let chunk = (count - done).min(BOUNCE_BYTES / 512);
        let bytes = chunk as usize * 512;
        let src = unsafe { (buf as *const u8).add(done as usize * 512) };
        unsafe { ptr::copy_nonoverlapping(src, phys_to_virt(c.bounce_phys), bytes) };
        compiler_fence(Ordering::SeqCst);
        if let Err(e) = submit_io(c, 0x01, lba + done as u64, chunk) {
            return e;
        }
        done += chunk;
    }
    0
}

unsafe extern "C" fn blk_flush(d: *mut BlkDev) -> i32 {
    if d.is_null() {
        return BLK_EINVAL;
    }
    let ctrl = unsafe { (*d).priv_ as *mut Controller };
    if ctrl.is_null() {
        return BLK_EINVAL;
    }
    let c = unsafe { &mut *ctrl };
    if c.failed {
        return BLK_EIO;
    }
    let cmd = Command {
        cdw0: 0x00,
        nsid: c.nsid,
        rsvd: [0; 2],
        mptr: 0,
        prp1: 0,
        prp2: 0,
        cdw10: 0,
        cdw11: 0,
        cdw12: 0,
        cdw13: 0,
        cdw14: 0,
        cdw15: 0,
    };
    execute(c, QueueKind::Io, cmd).map(|_| 0).unwrap_or_else(|e| e)
}

/* ---- Bring-up ---------------------------------------------------------- */

unsafe fn setup_queue(dstrd: u64, qid: u16, sq_phys: u64, cq_phys: u64) -> Queue {
    let stride = 4u64 << dstrd;
    Queue {
        sq: phys_to_virt(sq_phys) as *mut Command,
        cq: phys_to_virt(cq_phys) as *mut Completion,
        sq_tail: 0,
        cq_head: 0,
        phase: true,
        // SQ0 at 0x1000, CQ0 at 0x1004; each queue pair advances by 2*stride.
        sq_doorbell: 0x1000 + qid as u64 * 2 * stride,
        cq_doorbell: 0x1000 + qid as u64 * 2 * stride + stride,
        depth: QDEPTH,
    }
}

fn init_one(dev: *mut PciDev, index: u32, mmio_out: &mut *mut u8) -> Result<(), i32> {
    unsafe { pci_enable_bus_master_mem(dev) };
    let bar = unsafe { aether_pci_bar_mem(dev, 0) };
    if bar == 0 || bar & 0x3 != 0 {
        return Err(BLK_EINVAL);
    }
    if bar.checked_add(MMIO_SIZE).is_none() {
        return Err(BLK_EINVAL);
    }
    let window = MMIO_WINDOW + index as u64 * MMIO_SIZE;
    if unsafe { aether_map_mmio(window, bar, MMIO_SIZE) } != 0 {
        return Err(BLK_ENOMEM);
    }
    let mmio = window as *mut u8;
    *mmio_out = mmio;

    let cap = unsafe { raw_read64(mmio, CAP) };
    let mqes = (cap & 0xFFFF) as u32 + 1;
    let dstrd = (cap >> 32) & 0xF;
    let mpsmin = (cap >> 48) & 0xF;
    let css = (cap >> 37) & 0x1FF;
    if mqes < QDEPTH as u32 || dstrd > 4 || mpsmin != 0 || css & 1 == 0 {
        log("NVMe controller has queues too small");
        return Err(BLK_EINVAL);
    }
    let vs = unsafe { raw_read32(mmio, VS) };
    log("NVMe controller found");
    let _ = vs;

    // Reset and wait for the controller to become ready.
    unsafe { raw_write32(mmio, CC, 0) };
    let dl = deadline();
    if !wait_until(dl, || unsafe { raw_read32(mmio, CSTS) } & CSTS_RDY == 0) {
        log("NVMe reset timeout");
        return Err(BLK_ETIMEDOUT);
    }
    log("NVMe reset done");
    let admin_sq_phys = unsafe { pmm_alloc_contig(1) };
    let admin_cq_phys = unsafe { pmm_alloc_contig(1) };
    let io_sq_phys = unsafe { pmm_alloc_contig(1) };
    let io_cq_phys = unsafe { pmm_alloc_contig(1) };
    let bounce_phys = unsafe { pmm_alloc_contig(BOUNCE_PAGES as usize) };
    let prp_phys = unsafe { pmm_alloc_contig(1) };
    if admin_sq_phys == 0
        || admin_cq_phys == 0
        || io_sq_phys == 0
        || io_cq_phys == 0
        || bounce_phys == 0
        || prp_phys == 0
    {
        log("NVMe DMA allocation failed");
        return Err(BLK_ENOMEM);
    }
    let ctrl_phys = unsafe { pmm_alloc_contig(1) };
    if ctrl_phys == 0 {
        log("NVMe controller allocation failed");
        return Err(BLK_ENOMEM);
    }
    let ctrl = phys_to_virt(ctrl_phys) as *mut Controller;
    unsafe { ptr::write_bytes(ctrl, 0, 1) };

    unsafe {
        (*ctrl).mmio = mmio;
        (*ctrl).doorbell_stride = 4 << dstrd;
        (*ctrl).admin = setup_queue(dstrd, ADMIN_QID, admin_sq_phys, admin_cq_phys);
        (*ctrl).io = setup_queue(dstrd, IO_QID, io_sq_phys, io_cq_phys);
        (*ctrl).bounce_phys = bounce_phys;
        (*ctrl).prp_phys = prp_phys;
        (*ctrl).prp = phys_to_virt(prp_phys) as *mut u64;
        (*ctrl).cid = 0;
        (&mut (*ctrl).blk.name)[0..6].copy_from_slice(b"nvme0\0");
        (&mut (*ctrl).blk.model)[0..7].copy_from_slice(b"NVMe 0\0");
        (*ctrl).blk.sectors = 0;
        (*ctrl).blk.read = Some(blk_read);
        (*ctrl).blk.write = Some(blk_write);
        (*ctrl).blk.flush = Some(blk_flush);
        (*ctrl).blk.priv_ = ctrl as *mut c_void;
        (*ctrl).name = *b"nvme0\0\0\0\0\0\0\0\0\0\0\0";
    }

    unsafe {
        raw_write64(mmio, ASQ, admin_sq_phys);
        raw_write64(mmio, ACQ, admin_cq_phys);
        raw_write32(mmio, AQA, ((QDEPTH as u32 - 1) << 16) | (QDEPTH as u32 - 1));
        raw_write32(mmio, CC, CC_CSS_NVM | CC_IOSQES_64 | CC_IOCQES_16 | CC_EN);
    }
    let dl = deadline();
    if !wait_until(dl, || unsafe { raw_read32(mmio, CSTS) } & CSTS_RDY != 0) {
        log("NVMe controller did not become ready");
        return Err(BLK_ETIMEDOUT);
    }
    log("NVMe admin queue ready");
    if unsafe { raw_read32(mmio, CSTS) } & CSTS_CFS != 0 {
        log("NVMe controller is in fatal status");
        return Err(BLK_EIO);
    }

    let identify_phys = unsafe { pmm_alloc_contig(1) };
    let ctrl_ident = unsafe { pmm_alloc_contig(1) };
    if identify_phys == 0 || ctrl_ident == 0 {
        log("NVMe identify allocation failed");
        return Err(BLK_ENOMEM);
    }
    // Identify controller for the model string.
    if let Err(e) = unsafe { identify(&mut *ctrl, 1, 0, ctrl_ident) } {
        let _ = e;
        log("NVMe identify controller failed");
        return Err(e);
    }
    log("NVMe identify controller ok");
    let id = phys_to_virt(ctrl_ident);
    let mut model = [0u8; 41];
    let mut n = 0;
    for i in 0..40 {
        let b = unsafe { ptr::read(id.add(24 + i)) };
        if b == 0 {
            break;
        }
        model[n] = if b >= 32 && b < 127 { b } else { b'?' };
        n += 1;
    }
    let _ = n;

    // Find the first active namespace, then identify it.
    let mut nsid = 0u32;
    if unsafe { identify(&mut *ctrl, 2, 0, ctrl_ident) }.is_ok() {
        for i in 0..1024 {
            let v = unsafe { ptr::read((phys_to_virt(ctrl_ident) as *const u32).add(i)) };
            if v != 0 {
                nsid = v;
                break;
            }
        }
    }
    if nsid == 0 {
        nsid = 1;
    }
    if let Err(e) = unsafe { identify(&mut *ctrl, 0, nsid, identify_phys) } {
        let _ = e;
        log("NVMe identify namespace failed");
        return Err(e);
    }
    log("NVMe identify namespace ok");
    let idns = phys_to_virt(identify_phys);
    let nsze = unsafe { ptr::read_unaligned(idns as *const u64) };
    let flbas_raw = unsafe { ptr::read(idns.add(26)) };
    if flbas_raw & 0x10 != 0 {
        log("NVMe namespace uses separate metadata; skipping");
        return Err(BLK_EINVAL);
    }
    let flbas = flbas_raw & 0xF;
    let meta_size = unsafe {
        ptr::read_unaligned((idns.add(128 + flbas as usize * 4)) as *const u16)
    };
    if meta_size != 0 {
        log("NVMe namespace carries metadata; skipping");
        return Err(BLK_EINVAL);
    }
    let lba_shift = unsafe { ptr::read(idns.add(128 + flbas as usize * 4 + 2)) };
    if lba_shift != 9 {
        log("NVMe namespace is not 512-byte LBA; skipping");
        return Err(BLK_EINVAL);
    }
    if nsze == 0 {
        log("NVMe namespace is empty; skipping");
        return Err(BLK_EINVAL);
    }

    // Create the I/O completion and submission queues.
    if let Err(e) = unsafe { create_io_queues(&mut *ctrl, IO_QID, io_cq_phys, io_sq_phys, QDEPTH) } {
        let _ = e;
        log("NVMe create I/O queues failed");
        return Err(e);
    }
    log("NVMe I/O queues ready");
    unsafe {
        (*ctrl).nsid = nsid;
        (*ctrl).sectors = nsze;
        (*ctrl).blk.sectors = nsze;
        let mut m = [0u8; 41];
        m[..model.len()].copy_from_slice(&model);
        (*ctrl).blk.model = m;
        (*ctrl).blk.name = *b"nvme0\0\0\0\0\0\0\0\0\0\0\0";
        (*ctrl).model = m;
    }
    if unsafe { CONTROLLER } == ptr::null_mut() {
        unsafe { CONTROLLER = ctrl };
        unsafe { blk_register(&mut (*ctrl).blk) };
    }
    log("NVMe namespace ready");
    Ok(())
}

/// Called by the kernel after PCI and memory are up. Returns 0 when a
/// controller was registered, -1 otherwise.
#[no_mangle]
pub extern "C" fn nvme_init() -> i32 {
    log("nvme: scanning PCI");
    let mut registered = 0;
    for i in 0..4 {
        let dev = unsafe { pci_find_class(0x01, 0x08, 0x02, i) };
        if dev.is_null() {
            break;
        }
        log("nvme: controller found");
        let mut mmio = ptr::null_mut();
        match init_one(dev, i as u32, &mut mmio) {
            Ok(()) => {
                registered += 1;
                break;
            }
            Err(e) => {
                let _ = e;
                log("nvme: controller init failed");
                if !mmio.is_null() {
                    unsafe { raw_write32(mmio, CC, 0) };
                }
                unsafe { aether_pci_disable(dev) };
            }
        }
    }
    if registered > 0 { 0 } else { -1 }
}

#[panic_handler]
fn panic(_: &core::panic::PanicInfo) -> ! {
    unsafe { aether_kernel_panic() }
}
