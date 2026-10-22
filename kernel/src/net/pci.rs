// SPDX-License-Identifier: Apache-2.0
//! PCI configuration space through the legacy 0xCF8/0xCFC mechanism.
//!
//! Every supported machine (QEMU q35/i440fx, VMware Workstation) decodes the
//! legacy mechanism, so no ACPI MCFG parsing is needed to find a NIC. The
//! scan walks every bus behind bridges by brute force; it runs once at boot.

use core::arch::asm;

const CONFIG_ADDRESS: u16 = 0xcf8;
const CONFIG_DATA: u16 = 0xcfc;

/// Location of one PCI function.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Address {
    pub bus: u8,
    pub device: u8,
    pub function: u8,
}

/// One discovered function.
#[derive(Clone, Copy, Debug)]
pub struct Function {
    pub address: Address,
    pub vendor: u16,
    pub device: u16,
    pub class: u8,
    pub subclass: u8,
}

unsafe fn outl(port: u16, value: u32) {
    // SAFETY: The caller names a valid PCI configuration port.
    unsafe {
        asm!("out dx, eax", in("dx") port, in("eax") value, options(nomem, nostack, preserves_flags))
    };
}

unsafe fn inl(port: u16) -> u32 {
    let value: u32;
    // SAFETY: The caller names a valid PCI configuration port.
    unsafe {
        asm!("in eax, dx", in("dx") port, out("eax") value, options(nomem, nostack, preserves_flags))
    };
    value
}

fn select(address: Address, offset: u8) {
    let value = 0x8000_0000u32
        | (u32::from(address.bus) << 16)
        | (u32::from(address.device & 0x1f) << 11)
        | (u32::from(address.function & 0x7) << 8)
        | u32::from(offset & 0xfc);
    // SAFETY: 0xCF8 is the architectural configuration-address port; the
    // caller holds interrupts off so the address/data pair is not split.
    unsafe { outl(CONFIG_ADDRESS, value) };
}

/// Reads a 32-bit configuration register.
#[must_use]
pub fn read32(address: Address, offset: u8) -> u32 {
    let _guard = crate::arch::x86_64::timer::InterruptGuard::enter();
    select(address, offset);
    // SAFETY: 0xCFC is the configuration-data port selected above.
    unsafe { inl(CONFIG_DATA) }
}

/// Writes a 32-bit configuration register.
pub fn write32(address: Address, offset: u8, value: u32) {
    let _guard = crate::arch::x86_64::timer::InterruptGuard::enter();
    select(address, offset);
    // SAFETY: 0xCFC is the configuration-data port selected above.
    unsafe { outl(CONFIG_DATA, value) };
}

/// Reads a 16-bit configuration register.
#[must_use]
pub fn read16(address: Address, offset: u8) -> u16 {
    (read32(address, offset & !3) >> ((offset & 2) * 8)) as u16
}

/// Writes a 16-bit configuration register.
pub fn write16(address: Address, offset: u8, value: u16) {
    let aligned = offset & !3;
    let shift = (offset & 2) * 8;
    let old = read32(address, aligned);
    let new = (old & !(0xffff << shift)) | (u32::from(value) << shift);
    write32(address, aligned, new);
}

/// Calls `visit` for every present function, stopping when it returns
/// `true`.
pub fn find(mut visit: impl FnMut(&Function) -> bool) -> Option<Function> {
    for bus in 0..=255u8 {
        for device in 0..32u8 {
            let first = Address {
                bus,
                device,
                function: 0,
            };
            if read16(first, 0) == 0xffff {
                continue;
            }
            let functions = if read32(first, 0x0c) & 0x0080_0000 != 0 {
                8
            } else {
                1
            };
            for function in 0..functions {
                let address = Address {
                    bus,
                    device,
                    function,
                };
                let id = read32(address, 0);
                if id & 0xffff == 0xffff {
                    continue;
                }
                let class = read32(address, 0x08);
                let found = Function {
                    address,
                    vendor: id as u16,
                    device: (id >> 16) as u16,
                    class: (class >> 24) as u8,
                    subclass: (class >> 16) as u8,
                };
                if visit(&found) {
                    return Some(found);
                }
            }
        }
    }
    None
}

/// A memory BAR's physical base and size, or `None` for an I/O, empty, or
/// unprogrammed BAR. Sizing temporarily writes all-ones with memory decode
/// off, as the PCI specification describes.
#[must_use]
pub fn memory_bar(address: Address, index: u8) -> Option<(u64, u64)> {
    let offset = 0x10 + index * 4;
    let low = read32(address, offset);
    if low & 1 != 0 {
        return None;
    }
    let is_64 = (low >> 1) & 3 == 2;
    let command = read16(address, 0x04);
    write16(address, 0x04, command & !0x3);
    write32(address, offset, u32::MAX);
    let size_low = read32(address, offset);
    write32(address, offset, low);
    let (high, size_high) = if is_64 {
        let high = read32(address, offset + 4);
        write32(address, offset + 4, u32::MAX);
        let size_high = read32(address, offset + 4);
        write32(address, offset + 4, high);
        (high, size_high)
    } else {
        (0, u32::MAX)
    };
    write16(address, 0x04, command);
    let base = (u64::from(high) << 32) | u64::from(low & !0xf);
    let mask = (u64::from(size_high) << 32) | u64::from(size_low & !0xf);
    let size = (!mask).wrapping_add(1);
    if base == 0 || size == 0 {
        return None;
    }
    Some((base, size))
}

/// Enables memory decode and bus mastering, and masks legacy INTx: the
/// drivers poll.
pub fn enable_dma(address: Address) {
    let command = read16(address, 0x04);
    write16(address, 0x04, command | 0x2 | 0x4 | 0x400);
}
