// SPDX-License-Identifier: Apache-2.0
//! Intel 8254x/82574 (e1000/e1000e) Ethernet driver.
//!
//! These are the default NICs of QEMU (`e1000` on i440fx, `e1000e` on q35)
//! and VMware Workstation (`e1000`, `e1000e`). The driver uses only the
//! register subset the families share: legacy receive and transmit
//! descriptors, one unicast address filter, broadcast accept, and polling
//! with every interrupt masked.
//!
//! DMA model: the device only ever sees its two descriptor rings and the
//! fixed 2 KiB packet buffers the driver allocated for it. Frames are copied
//! between those buffers and the stack, so no kernel object outside the
//! driver's own pages is handed to the hardware, and a buffer is reused only
//! after the device reports it done. Every length the device writes is
//! bounds-checked before it is used.

use super::pci;
use aether_memory::PAGE_SIZE;
use core::ptr::{read_volatile, write_volatile};
use core::sync::atomic::{Ordering, fence};

/// Supported Intel device IDs.
const DEVICES: &[(u16, &str)] = &[
    (0x100e, "82540EM"),
    (0x100f, "82545EM"),
    (0x1011, "82545EM"),
    (0x1026, "82545GM"),
    (0x1076, "82541GI"),
    (0x107c, "82541PI"),
    (0x10d3, "82574L"),
];
const INTEL: u16 = 0x8086;

const CTRL: usize = 0x0000;
const STATUS: usize = 0x0008;
const ICR: usize = 0x00c0;
const IMC: usize = 0x00d8;
const RCTL: usize = 0x0100;
const TCTL: usize = 0x0400;
const TIPG: usize = 0x0410;
const RDBAL: usize = 0x2800;
const RDBAH: usize = 0x2804;
const RDLEN: usize = 0x2808;
const RDH: usize = 0x2810;
const RDT: usize = 0x2818;
const TDBAL: usize = 0x3800;
const TDBAH: usize = 0x3804;
const TDLEN: usize = 0x3808;
const TDH: usize = 0x3810;
const TDT: usize = 0x3818;
const MTA: usize = 0x5200;
const RAL0: usize = 0x5400;
const RAH0: usize = 0x5404;
/// 82574 PCIe control register; bit 22 must be set per the datasheet.
const GCR: usize = 0x5b00;

const CTRL_ASDE: u32 = 1 << 5;
const CTRL_SLU: u32 = 1 << 6;
const CTRL_RST: u32 = 1 << 26;
const CTRL_PHY_RST: u32 = 1 << 31;
const CTRL_LRST: u32 = 1 << 3;
const CTRL_ILOS: u32 = 1 << 7;
const CTRL_VME: u32 = 1 << 30;
const STATUS_LU: u32 = 1 << 1;

const RCTL_EN: u32 = 1 << 1;
const RCTL_BAM: u32 = 1 << 15;
const RCTL_SECRC: u32 = 1 << 26;
const TCTL_EN: u32 = 1 << 1;
const TCTL_PSP: u32 = 1 << 3;
const TCTL_CT: u32 = 0x0f << 4;
const TCTL_COLD: u32 = 0x40 << 12;

const RX_DD: u8 = 1 << 0;
const RX_EOP: u8 = 1 << 1;
const TX_EOP: u8 = 1 << 0;
const TX_IFCS: u8 = 1 << 1;
const TX_RS: u8 = 1 << 3;
const TX_DD: u8 = 1 << 0;

/// Descriptors per ring: one page of 16-byte descriptors.
pub const RING: usize = 256;
/// Packet buffer size; RCTL.BSIZE = 00 selects 2048 bytes.
pub const BUFFER: usize = 2048;
/// Largest Ethernet frame without the FCS.
pub const MAX_FRAME: usize = 1514;

/// The NIC failed to come up.
#[derive(Clone, Copy, Debug)]
pub enum Error {
    NoBar,
    Map,
    OutOfMemory,
    ResetTimeout,
}

/// A located, not yet initialized device.
pub struct Found {
    pub function: pci::Function,
    pub model: &'static str,
}

/// Finds the first supported Intel NIC.
#[must_use]
pub fn probe() -> Option<Found> {
    let mut model = "";
    let function = pci::find(|function| {
        if function.vendor != INTEL || function.class != 0x02 || function.subclass != 0x00 {
            return false;
        }
        match DEVICES.iter().find(|(id, _)| *id == function.device) {
            Some((_, name)) => {
                model = name;
                true
            }
            None => false,
        }
    })?;
    Some(Found { function, model })
}

/// Receive ring state.
pub struct Rx {
    regs: u64,
    ring: u64,
    buffers: [u64; RING],
    next: usize,
    /// Set while a multi-descriptor frame is being skipped.
    discarding: bool,
    pub dropped: u64,
}

/// Transmit ring state.
pub struct Tx {
    regs: u64,
    ring: u64,
    buffers: [u64; RING],
    next: usize,
}

/// One initialized device.
pub struct E1000 {
    pub rx: Rx,
    pub tx: Tx,
    regs: u64,
    pub mac: [u8; 6],
    pub model: &'static str,
}

fn read(regs: u64, offset: usize) -> u32 {
    // SAFETY: `regs` is the device's mapped, uncached register window and
    // `offset` is one of the 32-bit registers named above, inside the BAR.
    unsafe { read_volatile((regs as usize + offset) as *const u32) }
}

fn write(regs: u64, offset: usize, value: u32) {
    // SAFETY: As in `read`.
    unsafe { write_volatile((regs as usize + offset) as *mut u32, value) }
}

fn spin(iterations: u32) {
    for _ in 0..iterations {
        core::hint::spin_loop();
    }
}

/// Allocates one zeroed, identity-mapped frame for device use.
fn dma_page() -> Result<u64, Error> {
    let frame = crate::memory::with_frame_allocator(|frames| frames.allocate())
        .flatten()
        .ok_or(Error::OutOfMemory)?;
    let address = frame.start_address();
    // SAFETY: The frame was just allocated, is exclusively owned, and is
    // identity-mapped kernel RAM.
    unsafe { core::ptr::write_bytes(address as *mut u8, 0, PAGE_SIZE as usize) };
    Ok(address)
}

/// Allocates `RING` 2 KiB buffers, two per page.
fn dma_buffers() -> Result<[u64; RING], Error> {
    let mut buffers = [0u64; RING];
    for pair in buffers.chunks_mut(2) {
        let page = dma_page()?;
        pair[0] = page;
        if let Some(second) = pair.get_mut(1) {
            *second = page + BUFFER as u64;
        }
    }
    Ok(buffers)
}

impl E1000 {
    /// Resets and starts the device.
    pub fn init(found: &Found) -> Result<Self, Error> {
        let address = found.function.address;
        let (base, size) = pci::memory_bar(address, 0).ok_or(Error::NoBar)?;
        // The register file is 128 KiB; map no more than that.
        let length = size.min(128 * 1024);
        crate::memory::with_frame_allocator(|frames| {
            crate::memory::paging::map_kernel_mmio(frames, base, length)
        })
        .ok_or(Error::Map)?
        .map_err(|_| Error::Map)?;
        pci::enable_dma(address);
        let regs = base;

        // Mask interrupts, reset, and mask again: reset restores defaults.
        write(regs, IMC, u32::MAX);
        write(regs, CTRL, read(regs, CTRL) | CTRL_RST);
        spin(100_000);
        let mut waited = 0u32;
        while read(regs, CTRL) & CTRL_RST != 0 {
            if waited > 1_000 {
                return Err(Error::ResetTimeout);
            }
            spin(10_000);
            waited += 1;
        }
        write(regs, IMC, u32::MAX);
        let _ = read(regs, ICR);

        if found.function.device == 0x10d3 {
            write(regs, GCR, read(regs, GCR) | (1 << 22));
        }
        let ctrl = read(regs, CTRL) & !(CTRL_LRST | CTRL_PHY_RST | CTRL_ILOS | CTRL_VME);
        write(regs, CTRL, ctrl | CTRL_SLU | CTRL_ASDE);

        // The reset loads the permanent address into receive address 0.
        let low = read(regs, RAL0);
        let high = read(regs, RAH0);
        let mac = [
            low as u8,
            (low >> 8) as u8,
            (low >> 16) as u8,
            (low >> 24) as u8,
            high as u8,
            (high >> 8) as u8,
        ];
        // Address valid bit, so the unicast filter is in force.
        write(regs, RAH0, (high & 0xffff) | (1 << 31));
        for index in 0..128 {
            write(regs, MTA + index * 4, 0);
        }

        let rx_ring = dma_page()?;
        let rx_buffers = dma_buffers()?;
        for (index, buffer) in rx_buffers.iter().enumerate() {
            // SAFETY: `rx_ring` is a driver-owned page of RING descriptors.
            unsafe { write_volatile((rx_ring as usize + index * 16) as *mut u64, *buffer) };
        }
        write(regs, RDBAL, rx_ring as u32);
        write(regs, RDBAH, (rx_ring >> 32) as u32);
        write(regs, RDLEN, (RING * 16) as u32);
        write(regs, RDH, 0);
        write(regs, RDT, (RING - 1) as u32);
        write(regs, RCTL, RCTL_EN | RCTL_BAM | RCTL_SECRC);

        let tx_ring = dma_page()?;
        let tx_buffers = dma_buffers()?;
        for (index, buffer) in tx_buffers.iter().enumerate() {
            let descriptor = tx_ring as usize + index * 16;
            // SAFETY: `tx_ring` is a driver-owned page of RING descriptors.
            // Every descriptor starts done so the ring reads as empty.
            unsafe {
                write_volatile(descriptor as *mut u64, *buffer);
                write_volatile((descriptor + 12) as *mut u8, TX_DD);
            }
        }
        write(regs, TDBAL, tx_ring as u32);
        write(regs, TDBAH, (tx_ring >> 32) as u32);
        write(regs, TDLEN, (RING * 16) as u32);
        write(regs, TDH, 0);
        write(regs, TDT, 0);
        write(regs, TIPG, 0x0060_200a);
        write(regs, TCTL, TCTL_EN | TCTL_PSP | TCTL_CT | TCTL_COLD);

        Ok(Self {
            rx: Rx {
                regs,
                ring: rx_ring,
                buffers: rx_buffers,
                next: 0,
                discarding: false,
                dropped: 0,
            },
            tx: Tx {
                regs,
                ring: tx_ring,
                buffers: tx_buffers,
                next: 0,
            },
            regs,
            mac,
            model: found.model,
        })
    }

    /// Whether the PHY reports link.
    #[must_use]
    pub fn link_up(&self) -> bool {
        read(self.regs, STATUS) & STATUS_LU != 0
    }
}

impl Rx {
    fn descriptor(&self) -> usize {
        self.ring as usize + self.next * 16
    }

    /// Returns the next complete, error-free frame, skipping and recycling
    /// anything else. The slice stays valid until [`Rx::release`].
    pub fn peek(&mut self) -> Option<&[u8]> {
        loop {
            let descriptor = self.descriptor();
            // SAFETY: The descriptor lies in the driver's RX ring page.
            let status = unsafe { read_volatile((descriptor + 12) as *const u8) };
            if status & RX_DD == 0 {
                return None;
            }
            // Read the rest of the descriptor only after DD is observed.
            fence(Ordering::Acquire);
            // SAFETY: As above.
            let (length, errors) = unsafe {
                (
                    usize::from(read_volatile((descriptor + 8) as *const u16)),
                    read_volatile((descriptor + 13) as *const u8),
                )
            };
            let end = status & RX_EOP != 0;
            let skip = self.discarding || !end || errors != 0 || length == 0 || length > BUFFER;
            // A frame spanning descriptors is dropped through its last one.
            self.discarding = !end;
            if skip {
                self.dropped += 1;
                self.release();
                continue;
            }
            let buffer = self.buffers[self.next] as usize;
            // SAFETY: The buffer is a driver-owned 2 KiB DMA buffer the device
            // finished writing (DD set), and `length <= BUFFER`.
            return Some(unsafe { core::slice::from_raw_parts(buffer as *const u8, length) });
        }
    }

    /// Hands the current descriptor back to the device.
    pub fn release(&mut self) {
        let descriptor = self.descriptor();
        // SAFETY: The descriptor lies in the driver's RX ring page.
        unsafe {
            write_volatile(descriptor as *mut u64, self.buffers[self.next]);
            write_volatile((descriptor + 8) as *mut u64, 0);
        }
        fence(Ordering::Release);
        write(self.regs, RDT, self.next as u32);
        self.next = (self.next + 1) % RING;
    }
}

impl Tx {
    fn descriptor(&self) -> usize {
        self.ring as usize + self.next * 16
    }

    /// Whether the next descriptor is free.
    #[must_use]
    pub fn ready(&self) -> bool {
        // SAFETY: The descriptor lies in the driver's TX ring page.
        unsafe { read_volatile((self.descriptor() + 12) as *const u8) & TX_DD != 0 }
    }

    /// Fills the next buffer with `fill` and queues `length` bytes.
    pub fn send<R>(&mut self, length: usize, fill: impl FnOnce(&mut [u8]) -> R) -> R {
        let length = length.min(MAX_FRAME);
        let buffer = self.buffers[self.next] as usize;
        // SAFETY: The buffer is a driver-owned 2 KiB DMA buffer whose
        // descriptor is done (checked by `ready`), so the device is not
        // reading it.
        let frame = unsafe { core::slice::from_raw_parts_mut(buffer as *mut u8, length) };
        let result = fill(frame);
        let descriptor = self.descriptor();
        // SAFETY: The descriptor lies in the driver's TX ring page.
        unsafe {
            write_volatile(descriptor as *mut u64, self.buffers[self.next]);
            write_volatile((descriptor + 8) as *mut u16, length as u16);
            write_volatile((descriptor + 10) as *mut u8, 0);
            write_volatile((descriptor + 11) as *mut u8, TX_EOP | TX_IFCS | TX_RS);
            write_volatile((descriptor + 12) as *mut u8, 0);
        }
        fence(Ordering::Release);
        self.next = (self.next + 1) % RING;
        write(self.regs, TDT, self.next as u32);
        result
    }
}
