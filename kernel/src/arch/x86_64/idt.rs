// SPDX-License-Identifier: Apache-2.0
//! Interrupt Descriptor Table and CPU-exception reporting.
//!
//! Only the 32 architectural exception vectors are installed. Interrupts stay
//! disabled until the timer and interrupt controller work in Phase 2. The goal
//! here is diagnosability: a page fault or general-protection fault must print
//! its vector, error code, and instruction pointer instead of silently
//! resetting the machine.

use core::arch::asm;

use crate::serial_line;

const CODE_SELECTOR: u16 = 0x08;
const INTERRUPT_GATE: u8 = 0x8e;

#[repr(C)]
#[derive(Clone, Copy)]
struct IdtEntry {
    offset_low: u16,
    selector: u16,
    ist: u8,
    type_attr: u8,
    offset_mid: u16,
    offset_high: u32,
    reserved: u32,
}

impl IdtEntry {
    const fn missing() -> Self {
        Self {
            offset_low: 0,
            selector: 0,
            ist: 0,
            type_attr: 0,
            offset_mid: 0,
            offset_high: 0,
            reserved: 0,
        }
    }

    fn set_handler(&mut self, handler: u64) {
        self.offset_low = handler as u16;
        self.selector = CODE_SELECTOR;
        self.ist = 0;
        self.type_attr = INTERRUPT_GATE;
        self.offset_mid = (handler >> 16) as u16;
        self.offset_high = (handler >> 32) as u32;
        self.reserved = 0;
    }
}

#[repr(C, packed)]
struct DescriptorTablePointer {
    limit: u16,
    base: u64,
}

static mut IDT: [IdtEntry; 256] = [IdtEntry::missing(); 256];

core::arch::global_asm!(
    r#"
    .section .text
    .p2align 4
    .global isr_common
    .type isr_common, @function
isr_common:
    /* [rbp+0]=vector, [rbp+8]=error, [rbp+16]=RIP, [rbp+24]=CS, [rbp+32]=RFLAGS */
    mov rbp, rsp
    and rsp, -16
    mov rdi, [rbp]
    mov rsi, [rbp + 8]
    mov rdx, [rbp + 16]
    mov rcx, [rbp + 24]
    mov r8,  [rbp + 32]
    call {handler}
1:
    hlt
    jmp 1b

.macro isr_noerr num
    .global isr_\num
    .type isr_\num, @function
isr_\num:
    push 0
    push \num
    jmp isr_common
.endm

.macro isr_err num
    .global isr_\num
    .type isr_\num, @function
isr_\num:
    push \num
    jmp isr_common
.endm

.irp n, 0,1,2,3,4,5,6,7,9,15,16,18,19,20,22,23,24,25,26,27,28,31
    isr_noerr \n
.endr

.irp n, 8,10,11,12,13,14,17,21,29,30
    isr_err \n
.endr
    "#,
    handler = sym exception_handler,
);

unsafe extern "C" {
    fn isr_0();
    fn isr_1();
    fn isr_2();
    fn isr_3();
    fn isr_4();
    fn isr_5();
    fn isr_6();
    fn isr_7();
    fn isr_8();
    fn isr_9();
    fn isr_10();
    fn isr_11();
    fn isr_12();
    fn isr_13();
    fn isr_14();
    fn isr_15();
    fn isr_16();
    fn isr_17();
    fn isr_18();
    fn isr_19();
    fn isr_20();
    fn isr_21();
    fn isr_22();
    fn isr_23();
    fn isr_24();
    fn isr_25();
    fn isr_26();
    fn isr_27();
    fn isr_28();
    fn isr_29();
    fn isr_30();
    fn isr_31();
}

const HANDLERS: [unsafe extern "C" fn(); 32] = [
    isr_0, isr_1, isr_2, isr_3, isr_4, isr_5, isr_6, isr_7, isr_8, isr_9, isr_10, isr_11, isr_12,
    isr_13, isr_14, isr_15, isr_16, isr_17, isr_18, isr_19, isr_20, isr_21, isr_22, isr_23, isr_24,
    isr_25, isr_26, isr_27, isr_28, isr_29, isr_30, isr_31,
];

/// Installs the exception handlers and loads the IDT.
pub fn init() {
    // SAFETY: Boot is single-threaded, interrupts are disabled, and this is
    // the only code that mutates the static IDT.
    let idt = core::ptr::addr_of_mut!(IDT).cast::<IdtEntry>();
    for (index, handler) in HANDLERS.iter().enumerate() {
        // SAFETY: `index` is bounded by the 32-entry handler array and the
        // static IDT has 256 entries.
        unsafe { (*idt.add(index)).set_handler(*handler as usize as u64) };
    }

    let pointer = DescriptorTablePointer {
        limit: (core::mem::size_of::<[IdtEntry; 256]>() - 1) as u16,
        base: core::ptr::addr_of!(IDT) as u64,
    };

    // SAFETY: The table is fully initialized and lives for the program
    // lifetime.
    unsafe {
        asm!(
            "lidt [{pointer}]",
            pointer = in(reg) &pointer,
            options(readonly, nostack, preserves_flags)
        );
    }
}

/// Common exception entry point.
///
/// This function never returns. Phase 1 has no task to terminate, so a fault
/// stops the machine after producing a diagnostic.
extern "C" fn exception_handler(
    vector: u64,
    error: u64,
    instruction_pointer: u64,
    code_segment: u64,
    flags: u64,
) -> ! {
    let cr2: u64;
    // SAFETY: Reading CR2 is valid at CPL 0 and has no side effects. It is
    // meaningful for page faults and harmless for other vectors.
    unsafe {
        asm!(
            "mov {cr2}, cr2",
            cr2 = out(reg) cr2,
            options(nomem, nostack, preserves_flags)
        );
    }

    serial_line!(
        "KERNEL EXCEPTION: {} ({vector}) error={error:#x} rip={instruction_pointer:#018x} \
         cs={code_segment:#x} rflags={flags:#018x} cr2={cr2:#018x}",
        exception_name(vector)
    );
    serial_line!("AETHER_BOOT_FAILED reason=exception vector={vector}");
    crate::arch::x86_64::halt()
}

fn exception_name(vector: u64) -> &'static str {
    match vector {
        0 => "divide error",
        1 => "debug",
        2 => "non-maskable interrupt",
        3 => "breakpoint",
        4 => "overflow",
        5 => "bound range exceeded",
        6 => "invalid opcode",
        7 => "device not available",
        8 => "double fault",
        10 => "invalid TSS",
        11 => "segment not present",
        12 => "stack-segment fault",
        13 => "general protection fault",
        14 => "page fault",
        16 => "x87 floating-point exception",
        17 => "alignment check",
        18 => "machine check",
        19 => "SIMD floating-point exception",
        20 => "virtualization exception",
        21 => "control-protection exception",
        29 => "VMM communication exception",
        30 => "security exception",
        _ => "reserved or unknown exception",
    }
}
