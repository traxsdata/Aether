// SPDX-License-Identifier: Apache-2.0
//! Interrupt Descriptor Table, exceptions, timer, and syscall entry.
//!
//! The kernel installs all 32 architectural exception vectors, the PIC timer
//! IRQ, and a ring-3 `int 0x80` system call gate. Every path saves a complete
//! [`InterruptFrame`]; the timer and syscall paths may return a different frame
//! so the scheduler can switch address spaces. A kernel fault is fatal, while
//! a user fault terminates only the faulting task.

use core::arch::asm;

use crate::serial_line;

const CODE_SELECTOR: u16 = 0x08;
const INTERRUPT_GATE: u8 = 0x8e;
/// Present, DPL 3, 64-bit interrupt gate.
const USER_INTERRUPT_GATE: u8 = 0xee;
/// System call vector.
pub const SYSCALL_VECTOR: u8 = 0x80;

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

    fn set_user_handler(&mut self, handler: u64) {
        self.set_handler(handler);
        self.type_attr = USER_INTERRUPT_GATE;
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
    /* Common tail for exceptions: save the full context, dispatch to Rust,
       restore the frame selected by the dispatcher, and return. */
    .section .text
    .p2align 4
    .global isr_common
    .type isr_common, @function
isr_common:
    push rax
    push rbx
    push rcx
    push rdx
    push rsi
    push rdi
    push rbp
    push r8
    push r9
    push r10
    push r11
    push r12
    push r13
    push r14
    push r15

    mov rbp, rsp
    and rsp, -16
    mov rdi, rbp
    call {exception_handler}
    mov rsp, rax

    pop r15
    pop r14
    pop r13
    pop r12
    pop r11
    pop r10
    pop r9
    pop r8
    pop rbp
    pop rdi
    pop rsi
    pop rdx
    pop rcx
    pop rbx
    pop rax

    add rsp, 16
    iretq

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

/* IRQ0 (timer) and IRQ1 (keyboard) from the remapped PIC. */
.global irq_0
.type irq_0, @function
irq_0:
    push 0
    push 32
    jmp irq_common

.global irq_1
.type irq_1, @function
irq_1:
    push 0
    push 33
    jmp irq_common

.global irq_12
.type irq_12, @function
irq_12:
    push 0
    push 44
    jmp irq_common

/* Save the full interrupted context, call Rust, restore the selected frame. */
.p2align 4
.global irq_common
.type irq_common, @function
irq_common:
    push rax
    push rbx
    push rcx
    push rdx
    push rsi
    push rdi
    push rbp
    push r8
    push r9
    push r10
    push r11
    push r12
    push r13
    push r14
    push r15

    mov rbp, rsp
    and rsp, -16
    mov rdi, rbp
    call {irq_handler}
    mov rsp, rax

    pop r15
    pop r14
    pop r13
    pop r12
    pop r11
    pop r10
    pop r9
    pop r8
    pop rbp
    pop rdi
    pop rsi
    pop rdx
    pop rcx
    pop rbx
    pop rax

    add rsp, 16
    iretq

/* Linux `syscall` entry.
 *
 * The CPU enters with the user stack, so the stub first saves the user RSP in
 * a fixed kernel slot and switches to the task's kernel stack. Interrupts are
 * masked by IA32_FMASK. The frame matches InterruptFrame so the shared pop
 * sequence and iretq can return to ring 3.
 */
.section .bss
.p2align 3
.global linux_syscall_stack
linux_syscall_stack:
    .quad 0
.p2align 3
linux_syscall_user_rsp:
    .quad 0

.section .text
.global linux_syscall_entry
.type linux_syscall_entry, @function
linux_syscall_entry:
    mov [rip + linux_syscall_user_rsp], rsp
    mov rsp, [rip + linux_syscall_stack]

    push 0x1b
    push qword ptr [rip + linux_syscall_user_rsp]
    push r11
    push 0x23
    push rcx
    push 0
    push 128

    push rax
    push rbx
    push rcx
    push rdx
    push rsi
    push rdi
    push rbp
    push r8
    push r9
    push r10
    push r11
    push r12
    push r13
    push r14
    push r15

    mov rbp, rsp
    and rsp, -16
    mov rdi, rbp
    call {linux_syscall_handler}
    mov rsp, rax

    pop r15
    pop r14
    pop r13
    pop r12
    pop r11
    pop r10
    pop r9
    pop r8
    pop rbp
    pop rdi
    pop rsi
    pop rdx
    pop rcx
    pop rbx
    pop rax

    add rsp, 16
    iretq

/* Ring-3 system call gate. */
.global syscall_entry
.type syscall_entry, @function
syscall_entry:
    push 0
    push 128
    jmp syscall_common

.p2align 4
.global syscall_common
.type syscall_common, @function
syscall_common:
    push rax
    push rbx
    push rcx
    push rdx
    push rsi
    push rdi
    push rbp
    push r8
    push r9
    push r10
    push r11
    push r12
    push r13
    push r14
    push r15

    mov rbp, rsp
    and rsp, -16
    mov rdi, rbp
    call {syscall_handler}
    mov rsp, rax

    pop r15
    pop r14
    pop r13
    pop r12
    pop r11
    pop r10
    pop r9
    pop r8
    pop rbp
    pop rdi
    pop rsi
    pop rdx
    pop rcx
    pop rbx
    pop rax

    add rsp, 16
    iretq
    "#,
    exception_handler = sym exception_handler,
    irq_handler = sym irq_handler,
    syscall_handler = sym syscall_handler,
    linux_syscall_handler = sym crate::syscall::linux_dispatch,
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
    fn irq_0();
    fn irq_1();
    fn irq_12();
    fn syscall_entry();
    fn linux_syscall_entry();
}

/// Saved general-purpose and interrupt-return context.
///
/// Field order matches the push order in the assembly stubs, lowest address
/// first. `rsp` and `ss` are present only when the interrupted context was
/// ring 3; `iretq` ignores them for a ring-0 frame.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default)]
pub struct InterruptFrame {
    /// Saved r15.
    pub r15: u64,
    /// Saved r14.
    pub r14: u64,
    /// Saved r13.
    pub r13: u64,
    /// Saved r12.
    pub r12: u64,
    /// Saved r11.
    pub r11: u64,
    /// Saved r10.
    pub r10: u64,
    /// Saved r9.
    pub r9: u64,
    /// Saved r8.
    pub r8: u64,
    /// Saved rbp.
    pub rbp: u64,
    /// Saved rdi.
    pub rdi: u64,
    /// Saved rsi.
    pub rsi: u64,
    /// Saved rdx.
    pub rdx: u64,
    /// Saved rcx.
    pub rcx: u64,
    /// Saved rbx.
    pub rbx: u64,
    /// Saved rax.
    pub rax: u64,
    /// Interrupt vector (synthetic for IRQs).
    pub vector: u64,
    /// Error code (zero for IRQs).
    pub error: u64,
    /// Instruction pointer to resume.
    pub rip: u64,
    /// Code segment to resume.
    pub cs: u64,
    /// RFLAGS to resume.
    pub rflags: u64,
    /// User stack pointer; only meaningful when returning to ring 3.
    pub rsp: u64,
    /// User stack segment; only meaningful when returning to ring 3.
    pub ss: u64,
}

impl InterruptFrame {
    /// Returns `true` if the frame will return to privilege level 3.
    #[must_use]
    pub const fn is_user(&self) -> bool {
        self.cs & 3 == 3
    }
}

const HANDLERS: [unsafe extern "C" fn(); 32] = [
    isr_0, isr_1, isr_2, isr_3, isr_4, isr_5, isr_6, isr_7, isr_8, isr_9, isr_10, isr_11, isr_12,
    isr_13, isr_14, isr_15, isr_16, isr_17, isr_18, isr_19, isr_20, isr_21, isr_22, isr_23, isr_24,
    isr_25, isr_26, isr_27, isr_28, isr_29, isr_30, isr_31,
];

/// Updates the kernel stack used by the Linux `syscall` instruction.
///
/// # Safety
///
/// `top` must be the exclusive top of the current task's writable kernel
/// stack. The caller must serialize this with context switches.
pub unsafe fn set_linux_syscall_stack(top: u64) {
    unsafe extern "C" {
        static mut linux_syscall_stack: u64;
    }
    // SAFETY: The caller guarantees a valid stack and serialized access.
    unsafe {
        core::ptr::addr_of_mut!(linux_syscall_stack).write_unaligned(top);
    }
}

/// Installs the exception handlers, timer, and syscall gate, then loads the IDT.
pub fn init() {
    // SAFETY: Boot is single-threaded, interrupts are disabled, and this is
    // the only code that mutates the static IDT.
    let idt = core::ptr::addr_of_mut!(IDT).cast::<IdtEntry>();
    for (index, handler) in HANDLERS.iter().enumerate() {
        // SAFETY: `index` is bounded by the 32-entry handler array and the
        // static IDT has 256 entries.
        unsafe { (*idt.add(index)).set_handler(*handler as usize as u64) };
    }

    // IRQ0 (timer), IRQ1 (keyboard), and IRQ12 (mouse) are vectors 32, 33, and
    // 44 after the PIC remap.
    // SAFETY: All indices are inside the IDT.
    unsafe {
        (*idt.add(32)).set_handler(irq_0 as *const () as usize as u64);
        (*idt.add(33)).set_handler(irq_1 as *const () as usize as u64);
        (*idt.add(44)).set_handler(irq_12 as *const () as usize as u64);
    }

    // The syscall gate is callable from ring 3.
    // SAFETY: Index 128 is inside the IDT.
    unsafe {
        (*idt.add(usize::from(SYSCALL_VECTOR)))
            .set_user_handler(syscall_entry as *const () as usize as u64);
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
        crate::arch::x86_64::cpu::enable_syscall(
            linux_syscall_entry as *const () as usize as u64,
            crate::arch::x86_64::gdt::KERNEL_CODE_SELECTOR,
            crate::arch::x86_64::gdt::USER_CODE_SELECTOR,
        );
    }
}

/// Common exception entry point.
///
/// A kernel fault is fatal. A user fault terminates only the current task and
/// returns the next runnable frame, so a misbehaving application cannot take
/// the kernel down.
extern "C" fn exception_handler(frame: *mut InterruptFrame) -> *mut InterruptFrame {
    // Read fields individually. A ring-0 exception frame does not contain the
    // hardware `RSP`/`SS` slots that exist for a ring-3 transition, so copying
    // the whole `InterruptFrame` would read past the initialized part of the
    // stack.
    let saved_vector = unsafe { (*frame).vector };
    let saved_error = unsafe { (*frame).error };
    let saved_rip = unsafe { (*frame).rip };
    let saved_cs = unsafe { (*frame).cs };
    let saved_rflags = unsafe { (*frame).rflags };
    let is_user = unsafe { (*frame).is_user() };
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
        "KERNEL EXCEPTION: {} ({}) error={:#x} rip={:#018x} cs={:#x} rflags={:#018x} cr2={:#018x}",
        exception_name(saved_vector),
        saved_vector,
        saved_error,
        saved_rip,
        saved_cs,
        saved_rflags,
        cr2
    );

    if is_user {
        if saved_vector == 14
            && crate::scheduler::resolve_linux_demand_fault(frame, cr2, saved_error)
        {
            serial_line!("user: demand page mapped address={:#018x}", cr2);
            return frame;
        }
        serial_line!(
            "user: fault terminated task vector={} rip={:#018x} cr2={:#018x}",
            saved_vector,
            saved_rip,
            cr2
        );
        return crate::scheduler::on_user_fault(frame, saved_vector);
    }

    serial_line!(
        "AETHER_BOOT_FAILED reason=exception vector={}",
        saved_vector
    );
    crate::arch::x86_64::halt()
}

/// PIC interrupt entry, called from `irq_common`.
extern "C" fn irq_handler(frame: *mut InterruptFrame) -> *mut InterruptFrame {
    // SAFETY: The assembly stub passes a complete live frame.
    let vector = unsafe { (*frame).vector };
    crate::arch::x86_64::timer::acknowledge_vector(vector);
    match vector {
        32 => crate::scheduler::on_tick(frame),
        33 => {
            crate::input::on_irq();
            frame
        }
        44 => {
            crate::input::on_mouse_irq();
            frame
        }
        _ => crate::arch::x86_64::halt(),
    }
}

/// System call entry, called from `syscall_common`.
extern "C" fn syscall_handler(frame: *mut InterruptFrame) -> *mut InterruptFrame {
    crate::syscall::dispatch(frame)
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
