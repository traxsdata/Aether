// SPDX-License-Identifier: Apache-2.0
//! Global Descriptor Table setup.
//!
//! Long mode does not use segmentation for addressing, but it still requires a
//! valid code descriptor for the current privilege level. UEFI normally leaves
//! a usable GDT installed; we replace it so the kernel does not depend on
//! firmware state.

use core::arch::asm;

const CODE_SELECTOR: u16 = 0x08;
const DATA_SELECTOR: u16 = 0x10;

/// Null, 64-bit ring-0 code, and ring-0 data descriptors.
static GDT: [u64; 3] = [
    0,
    // Present, DPL 0, code, non-conforming, readable, long mode.
    0x00af_9a00_0000_0000,
    // Present, DPL 0, data, writable.
    0x00af_9200_0000_0000,
];

#[repr(C, packed)]
struct DescriptorTablePointer {
    limit: u16,
    base: u64,
}

/// Loads the kernel GDT and reloads every segment register.
pub fn init() {
    let pointer = DescriptorTablePointer {
        limit: (core::mem::size_of_val(&GDT) - 1) as u16,
        base: GDT.as_ptr() as u64,
    };

    // SAFETY: The GDT and pointer are static and live for the program
    // lifetime. Reloading CS via a far return is required in long mode; the
    // target label is in the same code segment at ring 0. Interrupts are
    // disabled during boot, so no interrupt can observe a half-updated state.
    unsafe {
        asm!(
            "lgdt [{pointer}]",
            "push {code}",
            "lea rax, [rip + 2f]",
            "push rax",
            "retfq",
            "2:",
            "mov ax, {data}",
            "mov ds, ax",
            "mov es, ax",
            "mov fs, ax",
            "mov gs, ax",
            "mov ss, ax",
            pointer = in(reg) &pointer,
            code = const CODE_SELECTOR,
            data = const DATA_SELECTOR,
            out("rax") _,
            options(preserves_flags),
        );
    }
}
