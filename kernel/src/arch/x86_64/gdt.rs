// SPDX-License-Identifier: Apache-2.0
//! Global Descriptor Table, Task State Segment, and privilege boundaries.
//!
//! Long mode does not use segmentation for addressing, but it still requires a
//! valid code descriptor for the current privilege level and a TSS before the
//! CPU can enter ring 3. UEFI normally leaves a usable GDT installed; we
//! replace it so the kernel does not depend on firmware state.
//!
//! The TSS supplies `RSP0`, the kernel stack the CPU switches to when an
//! interrupt or exception arrives from user mode. The scheduler updates it on
//! every context switch, so each process has a private kernel stack.

use core::arch::asm;

/// Kernel code selector.
pub const KERNEL_CODE_SELECTOR: u16 = 0x08;
/// Kernel data selector.
pub const KERNEL_DATA_SELECTOR: u16 = 0x10;
/// User data selector, requested at privilege level 3.
pub const USER_DATA_SELECTOR: u16 = 0x1b;
/// User code selector, requested at privilege level 3.
pub const USER_CODE_SELECTOR: u16 = 0x23;
/// Task State Segment selector.
pub const TSS_SELECTOR: u16 = 0x28;

/// A 64-bit Task State Segment.
#[repr(C, packed)]
pub struct TaskStateSegment {
    reserved0: u32,
    /// Stack pointer loaded when the CPU enters ring 0 from ring 3.
    pub rsp0: u64,
    rsp1: u64,
    rsp2: u64,
    reserved1: u64,
    ist: [u64; 7],
    reserved2: u64,
    reserved3: u16,
    iomap_base: u16,
}

impl TaskStateSegment {
    const fn new() -> Self {
        Self {
            reserved0: 0,
            rsp0: 0,
            rsp1: 0,
            rsp2: 0,
            reserved1: 0,
            ist: [0; 7],
            reserved2: 0,
            reserved3: 0,
            iomap_base: core::mem::size_of::<Self>() as u16,
        }
    }
}

static mut TSS: TaskStateSegment = TaskStateSegment::new();

/// Null, ring-0 code/data, ring-3 data/code, and a TSS descriptor.
const GDT: [u64; 7] = [
    0,
    // Present, DPL 0, code, non-conforming, readable, long mode.
    0x00af_9a00_0000_0000,
    // Present, DPL 0, data, writable.
    0x00af_9200_0000_0000,
    // Present, DPL 3, data, writable.
    0x00cf_f200_0000_0000,
    // Present, DPL 3, code, non-conforming, readable, long mode.
    0x00af_fa00_0000_0000,
    // TSS low descriptor, filled in at runtime.
    0,
    // TSS high descriptor, filled in at runtime.
    0,
];

#[repr(C, packed)]
struct DescriptorTablePointer {
    limit: u16,
    base: u64,
}

fn tss_descriptor(base: u64, limit: u16) -> (u64, u64) {
    let low = u64::from(limit)
        | ((base & 0x00ff_ffff) << 16)
        | (0x89u64 << 40)
        | ((u64::from(limit) >> 16 & 0xf) << 48)
        | (((base >> 24) & 0xff) << 56);
    (low, base >> 32)
}

/// Loads the kernel GDT, installs the TSS, and reloads every segment register.
pub fn init() {
    // SAFETY: Boot is single-threaded, interrupts are disabled, and this is the
    // only code that mutates the static GDT/TSS before the scheduler starts.
    let (tss_low, tss_high) = tss_descriptor(core::ptr::addr_of!(TSS) as u64, 103);
    let mut gdt = GDT;
    gdt[5] = tss_low;
    gdt[6] = tss_high;

    // The GDT must remain at a stable address for the lifetime of the kernel.
    // Copy the template into the writable static slot below.

    // SAFETY: `GDT_RUNTIME` is a static, writable table. Writing it before the
    // first user task exists cannot race with another CPU or interrupt.
    unsafe {
        let destination = core::ptr::addr_of_mut!(GDT_RUNTIME).cast::<u64>();
        for (index, value) in gdt.iter().enumerate() {
            core::ptr::write(destination.add(index), *value);
        }
    }

    let pointer = DescriptorTablePointer {
        limit: (core::mem::size_of_val(&gdt) - 1) as u16,
        base: core::ptr::addr_of!(GDT_RUNTIME) as u64,
    };

    // SAFETY: The table and pointer are static and live for the program
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
            "mov ax, {tss}",
            "ltr ax",
            pointer = in(reg) &pointer,
            code = const KERNEL_CODE_SELECTOR,
            data = const KERNEL_DATA_SELECTOR,
            tss = const TSS_SELECTOR,
            out("rax") _,
            options(preserves_flags),
        );
    }
}

/// Runtime copy of the GDT with the TSS descriptor patched in.
static mut GDT_RUNTIME: [u64; 7] = GDT;

/// Sets the kernel stack used for the next ring-3 to ring-0 transition.
///
/// # Safety
///
/// `top` must be the exclusive top of a writable, 16-byte aligned kernel stack
/// owned by the task that will run next. The caller must mask interrupts or
/// otherwise serialize the update with context switches.
pub unsafe fn set_kernel_stack(top: u64) {
    // SAFETY: The caller guarantees a valid stack and serialized access to the
    // TSS.
    unsafe {
        let tss = core::ptr::addr_of_mut!(TSS);
        core::ptr::addr_of_mut!((*tss).rsp0).write_unaligned(top);
    }
}
