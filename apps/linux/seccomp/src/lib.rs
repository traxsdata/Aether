// SPDX-License-Identifier: Apache-2.0
//! Classic BPF for Linux seccomp filters.
//!
//! A seccomp filter is a classic BPF program that reads a 64-byte
//! `struct seccomp_data` and returns an action. This crate checks a program
//! the way Linux `seccomp_check_filter` does and runs it:
//!
//! - Only the instructions Linux allows in a seccomp filter are accepted:
//!   absolute 32-bit loads from `seccomp_data`, immediate and scratch-memory
//!   loads and stores, 32-bit arithmetic, forward jumps, and returns. Packet
//!   loads (`BPF_IND`, `BPF_MSH`, byte and half-word sizes) are refused.
//! - Every jump is forward and lands inside the program, and the last
//!   instruction returns, so a run takes at most one step per instruction.
//! - Division by a constant zero and constant shifts of 32 or more are
//!   refused. Division by a zero `X` at run time ends the program with 0,
//!   which seccomp treats as `SECCOMP_RET_KILL_THREAD`.
//! - Scratch memory starts zeroed, so a program that reads a slot before
//!   storing it sees 0 rather than stale data.
//!
//! [`Action`] decodes a return value and [`combine`] applies the Linux
//! precedence rule when several filters are stacked.

#![no_std]

#[cfg(test)]
extern crate std;

/// Largest program Linux accepts (`BPF_MAXINSNS`).
pub const MAX_INSTRUCTIONS: usize = 4096;
/// Size of `struct seccomp_data`.
pub const DATA_BYTES: usize = 64;
/// Scratch memory words (`BPF_MEMWORDS`).
pub const MEMORY_WORDS: usize = 16;
/// `AUDIT_ARCH_X86_64`.
pub const AUDIT_ARCH_X86_64: u32 = 0xc000_003e;

/// `SECCOMP_RET_KILL_PROCESS`.
pub const RET_KILL_PROCESS: u32 = 0x8000_0000;
/// `SECCOMP_RET_KILL_THREAD`.
pub const RET_KILL_THREAD: u32 = 0x0000_0000;
/// `SECCOMP_RET_TRAP`.
pub const RET_TRAP: u32 = 0x0003_0000;
/// `SECCOMP_RET_ERRNO`.
pub const RET_ERRNO: u32 = 0x0005_0000;
/// `SECCOMP_RET_USER_NOTIF`.
pub const RET_USER_NOTIF: u32 = 0x7fc0_0000;
/// `SECCOMP_RET_TRACE`.
pub const RET_TRACE: u32 = 0x7ff0_0000;
/// `SECCOMP_RET_LOG`.
pub const RET_LOG: u32 = 0x7ffc_0000;
/// `SECCOMP_RET_ALLOW`.
pub const RET_ALLOW: u32 = 0x7fff_0000;
/// `SECCOMP_RET_ACTION_FULL`.
pub const RET_ACTION_FULL: u32 = 0xffff_0000;
/// `SECCOMP_RET_DATA`.
pub const RET_DATA: u32 = 0x0000_ffff;

// Instruction classes.
const LD: u16 = 0x00;
const LDX: u16 = 0x01;
const ST: u16 = 0x02;
const STX: u16 = 0x03;
const ALU: u16 = 0x04;
const JMP: u16 = 0x05;
const RET: u16 = 0x06;
const MISC: u16 = 0x07;
// Load sizes and modes.
const W: u16 = 0x00;
const IMM: u16 = 0x00;
const ABS: u16 = 0x20;
const MEM: u16 = 0x60;
const LEN: u16 = 0x80;
// ALU and jump operations.
const ADD: u16 = 0x00;
const SUB: u16 = 0x10;
const MUL: u16 = 0x20;
const DIV: u16 = 0x30;
const OR: u16 = 0x40;
const AND: u16 = 0x50;
const LSH: u16 = 0x60;
const RSH: u16 = 0x70;
const NEG: u16 = 0x80;
const XOR: u16 = 0xa0;
const JA: u16 = 0x00;
const JEQ: u16 = 0x10;
const JGT: u16 = 0x20;
const JGE: u16 = 0x30;
const JSET: u16 = 0x40;
// Operand sources and return values.
const K: u16 = 0x00;
const X: u16 = 0x08;
const RET_A: u16 = 0x10;
const TAX: u16 = 0x00;
const TXA: u16 = 0x80;

/// One `struct sock_filter`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Instruction {
    pub code: u16,
    pub jt: u8,
    pub jf: u8,
    pub k: u32,
}

impl Instruction {
    /// Decodes the 8-byte native-endian `struct sock_filter`.
    #[must_use]
    pub fn from_bytes(bytes: [u8; 8]) -> Self {
        Self {
            code: u16::from_ne_bytes([bytes[0], bytes[1]]),
            jt: bytes[2],
            jf: bytes[3],
            k: u32::from_ne_bytes([bytes[4], bytes[5], bytes[6], bytes[7]]),
        }
    }

    /// Builds a statement (`BPF_STMT`).
    #[must_use]
    pub const fn statement(code: u16, k: u32) -> Self {
        Self {
            code,
            jt: 0,
            jf: 0,
            k,
        }
    }

    /// Builds a conditional jump (`BPF_JUMP`).
    #[must_use]
    pub const fn jump(code: u16, k: u32, jt: u8, jf: u8) -> Self {
        Self { code, jt, jf, k }
    }
}

/// Why a program was refused.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Invalid {
    /// Empty, or longer than [`MAX_INSTRUCTIONS`].
    Length,
    /// An opcode seccomp does not allow.
    Opcode { at: usize },
    /// A load outside `seccomp_data`, or not 4-byte aligned.
    Offset { at: usize },
    /// A scratch-memory index of 16 or more.
    Memory { at: usize },
    /// A jump past the end of the program.
    Jump { at: usize },
    /// Division by a constant zero, or a constant shift of 32 or more.
    Constant { at: usize },
    /// The last instruction does not return.
    NoReturn,
}

/// Checks a program the way Linux `seccomp_check_filter` and
/// `bpf_check_classic` do.
///
/// # Errors
///
/// Returns the first reason the program cannot run as a seccomp filter.
pub fn validate(program: &[Instruction]) -> Result<(), Invalid> {
    if program.is_empty() || program.len() > MAX_INSTRUCTIONS {
        return Err(Invalid::Length);
    }
    let len = program.len();
    for (at, instruction) in program.iter().enumerate() {
        let code = instruction.code;
        let k = instruction.k;
        match code {
            c if c == LD | W | ABS => {
                if k as usize >= DATA_BYTES || !k.is_multiple_of(4) {
                    return Err(Invalid::Offset { at });
                }
            }
            c if c == LD | W | LEN || c == LDX | W | LEN => {}
            c if c == LD | IMM || c == LDX | IMM => {}
            c if c == LD | MEM || c == LDX | MEM || c == ST || c == STX => {
                if k as usize >= MEMORY_WORDS {
                    return Err(Invalid::Memory { at });
                }
            }
            c if c == ALU | NEG => {}
            c if c & 0x07 == ALU => {
                let op = c & 0xf0;
                let source = c & 0x08;
                if c & !0xf8 != ALU
                    || !matches!(op, ADD | SUB | MUL | DIV | OR | AND | LSH | RSH | XOR)
                {
                    return Err(Invalid::Opcode { at });
                }
                if source == K && (op == DIV && k == 0 || matches!(op, LSH | RSH) && k >= 32) {
                    return Err(Invalid::Constant { at });
                }
            }
            c if c == JMP | JA => {
                if (k as usize) >= len - at - 1 {
                    return Err(Invalid::Jump { at });
                }
            }
            c if c & 0x07 == JMP => {
                let op = c & 0xf0;
                if c & !0xf8 != JMP || !matches!(op, JEQ | JGT | JGE | JSET) {
                    return Err(Invalid::Opcode { at });
                }
                let room = len - at - 1;
                if usize::from(instruction.jt) >= room || usize::from(instruction.jf) >= room {
                    return Err(Invalid::Jump { at });
                }
            }
            c if c == RET | K || c == RET | RET_A => {}
            c if c == MISC | TAX || c == MISC | TXA => {}
            _ => return Err(Invalid::Opcode { at }),
        }
    }
    match program[len - 1].code {
        c if c == RET | K || c == RET | RET_A => Ok(()),
        _ => Err(Invalid::NoReturn),
    }
}

/// `struct seccomp_data` for one system call.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Data {
    pub nr: i32,
    pub arch: u32,
    pub instruction_pointer: u64,
    pub args: [u64; 6],
}

impl Data {
    /// The native-endian layout a filter's absolute loads read.
    #[must_use]
    pub fn to_bytes(&self) -> [u8; DATA_BYTES] {
        let mut bytes = [0u8; DATA_BYTES];
        bytes[0..4].copy_from_slice(&self.nr.to_ne_bytes());
        bytes[4..8].copy_from_slice(&self.arch.to_ne_bytes());
        bytes[8..16].copy_from_slice(&self.instruction_pointer.to_ne_bytes());
        for (index, arg) in self.args.iter().enumerate() {
            let at = 16 + index * 8;
            bytes[at..at + 8].copy_from_slice(&arg.to_ne_bytes());
        }
        bytes
    }
}

/// Runs a program that passed [`validate`] and returns its result.
#[must_use]
pub fn run(program: &[Instruction], data: &[u8; DATA_BYTES]) -> u32 {
    let mut a: u32 = 0;
    let mut x: u32 = 0;
    let mut memory = [0u32; MEMORY_WORDS];
    let mut pc = 0usize;
    // Jumps only go forward, so this visits each instruction at most once.
    while let Some(instruction) = program.get(pc) {
        pc += 1;
        let k = instruction.k;
        let code = instruction.code;
        match code & 0x07 {
            LD | LDX => {
                let value = match code & 0xe0 {
                    ABS => {
                        let at = k as usize;
                        data.get(at..at + 4).map_or(0, |bytes| {
                            u32::from_ne_bytes([bytes[0], bytes[1], bytes[2], bytes[3]])
                        })
                    }
                    MEM => memory.get(k as usize).copied().unwrap_or(0),
                    LEN => DATA_BYTES as u32,
                    _ => k,
                };
                if code & 0x07 == LD {
                    a = value;
                } else {
                    x = value;
                }
            }
            ST => {
                if let Some(slot) = memory.get_mut(k as usize) {
                    *slot = a;
                }
            }
            STX => {
                if let Some(slot) = memory.get_mut(k as usize) {
                    *slot = x;
                }
            }
            ALU => {
                let operand = if code & X != 0 { x } else { k };
                a = match code & 0xf0 {
                    ADD => a.wrapping_add(operand),
                    SUB => a.wrapping_sub(operand),
                    MUL => a.wrapping_mul(operand),
                    DIV => {
                        if operand == 0 {
                            return 0;
                        }
                        a / operand
                    }
                    OR => a | operand,
                    AND => a & operand,
                    LSH => a.wrapping_shl(operand),
                    RSH => a.wrapping_shr(operand),
                    NEG => a.wrapping_neg(),
                    XOR => a ^ operand,
                    _ => return 0,
                };
            }
            JMP => {
                let operand = if code & X != 0 { x } else { k };
                let taken = match code & 0xf0 {
                    JA => {
                        pc += k as usize;
                        continue;
                    }
                    JEQ => a == operand,
                    JGT => a > operand,
                    JGE => a >= operand,
                    JSET => a & operand != 0,
                    _ => return 0,
                };
                pc += usize::from(if taken {
                    instruction.jt
                } else {
                    instruction.jf
                });
            }
            RET => return if code & RET_A != 0 { a } else { k },
            MISC => {
                if code & TXA != 0 {
                    a = x;
                } else {
                    x = a;
                }
            }
            _ => return 0,
        }
    }
    0
}

/// What a seccomp return value asks for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Action {
    KillProcess,
    KillThread,
    /// Raise `SIGSYS`; the value is reported in `si_errno`.
    Trap(u16),
    /// Fail the call with this errno.
    Errno(u16),
    UserNotif,
    /// Hand the call to a tracer; the value is the tracer's event message.
    Trace(u16),
    Log,
    Allow,
}

impl Action {
    /// Decodes a filter result. An unknown action kills the process, as on
    /// Linux.
    #[must_use]
    pub fn from_result(result: u32) -> Self {
        let data = (result & RET_DATA) as u16;
        match result & RET_ACTION_FULL {
            RET_KILL_THREAD => Self::KillThread,
            RET_TRAP => Self::Trap(data),
            RET_ERRNO => Self::Errno(data),
            RET_USER_NOTIF => Self::UserNotif,
            RET_TRACE => Self::Trace(data),
            RET_LOG => Self::Log,
            RET_ALLOW => Self::Allow,
            _ => Self::KillProcess,
        }
    }
}

/// Picks the result that wins when two stacked filters disagree: the lower
/// action, compared as a signed value, so `KILL_PROCESS` beats everything
/// and `ALLOW` loses to everything. On a tie the earlier result is kept.
#[must_use]
pub fn combine(current: u32, next: u32) -> u32 {
    let rank = |result: u32| (result & RET_ACTION_FULL) as i32;
    if rank(next) < rank(current) {
        next
    } else {
        current
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::vec::Vec;

    const LD_NR: Instruction = Instruction::statement(LD | W | ABS, 0);
    const LD_ARCH: Instruction = Instruction::statement(LD | W | ABS, 4);

    fn ret(k: u32) -> Instruction {
        Instruction::statement(RET | K, k)
    }

    fn data(nr: i32, args: [u64; 6]) -> [u8; DATA_BYTES] {
        Data {
            nr,
            arch: AUDIT_ARCH_X86_64,
            instruction_pointer: 0x40_1000,
            args,
        }
        .to_bytes()
    }

    /// The usual arch check, then deny `nr` with `errno`, allow the rest.
    fn deny(nr: u32, errno: u32) -> Vec<Instruction> {
        std::vec![
            LD_ARCH,
            Instruction::jump(JMP | JEQ | K, AUDIT_ARCH_X86_64, 1, 0),
            ret(RET_KILL_PROCESS),
            LD_NR,
            Instruction::jump(JMP | JEQ | K, nr, 0, 1),
            ret(RET_ERRNO | errno),
            ret(RET_ALLOW),
        ]
    }

    #[test]
    fn denies_one_call_and_allows_the_rest() {
        let program = deny(41, 13);
        validate(&program).unwrap();
        assert_eq!(run(&program, &data(41, [0; 6])), RET_ERRNO | 13);
        assert_eq!(run(&program, &data(0, [0; 6])), RET_ALLOW);
        let mut other_arch = data(41, [0; 6]);
        other_arch[4..8].copy_from_slice(&0x4000_0003u32.to_ne_bytes());
        assert_eq!(run(&program, &other_arch), RET_KILL_PROCESS);
    }

    #[test]
    fn reads_both_halves_of_an_argument() {
        // Trap when the high half of arg 1 is set, as Chromium's 64-bit
        // argument checks do.
        let program = std::vec![
            Instruction::statement(LD | W | ABS, 16 + 8 + 4),
            Instruction::jump(JMP | JEQ | K, 0, 1, 0),
            ret(RET_TRAP | 7),
            Instruction::statement(LD | W | ABS, 16 + 8),
            Instruction::jump(JMP | JSET | K, 0x8, 0, 1),
            ret(RET_ERRNO | 1),
            ret(RET_ALLOW),
        ];
        validate(&program).unwrap();
        assert_eq!(
            run(&program, &data(9, [0, 1 << 32, 0, 0, 0, 0])),
            RET_TRAP | 7
        );
        assert_eq!(run(&program, &data(9, [0, 0x8, 0, 0, 0, 0])), RET_ERRNO | 1);
        assert_eq!(run(&program, &data(9, [0, 0x4, 0, 0, 0, 0])), RET_ALLOW);
    }

    #[test]
    fn arithmetic_scratch_memory_and_registers() {
        let program = std::vec![
            LD_NR,
            Instruction::statement(ALU | ADD | K, 3),
            Instruction::statement(ST, 2),
            Instruction::statement(LDX | IMM, 4),
            Instruction::statement(ALU | MUL | X, 0),
            Instruction::statement(MISC | TAX, 0),
            Instruction::statement(LD | MEM, 2),
            Instruction::statement(ALU | XOR | X, 0),
            Instruction::statement(ALU | RSH | K, 1),
            Instruction::statement(ALU | OR | K, RET_ERRNO),
            Instruction::statement(RET | RET_A, 0),
        ];
        validate(&program).unwrap();
        // nr 5: A = 8, M[2] = 8, A = 32, X = 32, A = 8 ^ 32 = 40, >> 1 = 20.
        assert_eq!(run(&program, &data(5, [0; 6])), RET_ERRNO | 20);
    }

    #[test]
    fn division_by_a_zero_register_returns_kill() {
        let program = std::vec![
            Instruction::statement(LD | IMM, 10),
            Instruction::statement(LDX | IMM, 0),
            Instruction::statement(ALU | DIV | X, 0),
            ret(RET_ALLOW),
        ];
        validate(&program).unwrap();
        assert_eq!(run(&program, &data(0, [0; 6])), RET_KILL_THREAD);
    }

    #[test]
    fn unread_scratch_memory_is_zero_and_len_is_the_data_size() {
        let program = std::vec![
            Instruction::statement(LD | MEM, 15),
            Instruction::statement(LDX | W | LEN, 0),
            Instruction::statement(ALU | ADD | X, 0),
            Instruction::statement(RET | RET_A, 0),
        ];
        validate(&program).unwrap();
        assert_eq!(run(&program, &data(0, [0; 6])), DATA_BYTES as u32);
    }

    #[test]
    fn refuses_programs_linux_refuses() {
        assert_eq!(validate(&[]), Err(Invalid::Length));
        let long = std::vec![ret(RET_ALLOW); MAX_INSTRUCTIONS + 1];
        assert_eq!(validate(&long), Err(Invalid::Length));
        // Packet loads and byte-sized loads.
        for code in [LD | 0x40, LD | 0x10 | ABS, LDX | 0xa0, LD | 0x08 | ABS] {
            assert_eq!(
                validate(&[Instruction::statement(code, 0), ret(RET_ALLOW)]),
                Err(Invalid::Opcode { at: 0 })
            );
        }
        // MOD is not a seccomp instruction.
        assert_eq!(
            validate(&[Instruction::statement(ALU | 0x90 | K, 3), ret(RET_ALLOW)]),
            Err(Invalid::Opcode { at: 0 })
        );
        assert_eq!(
            validate(&[Instruction::statement(LD | W | ABS, 64), ret(RET_ALLOW)]),
            Err(Invalid::Offset { at: 0 })
        );
        assert_eq!(
            validate(&[Instruction::statement(LD | W | ABS, 2), ret(RET_ALLOW)]),
            Err(Invalid::Offset { at: 0 })
        );
        assert_eq!(
            validate(&[Instruction::statement(ST, 16), ret(RET_ALLOW)]),
            Err(Invalid::Memory { at: 0 })
        );
        assert_eq!(
            validate(&[Instruction::statement(ALU | DIV | K, 0), ret(RET_ALLOW)]),
            Err(Invalid::Constant { at: 0 })
        );
        assert_eq!(
            validate(&[Instruction::statement(ALU | LSH | K, 32), ret(RET_ALLOW)]),
            Err(Invalid::Constant { at: 0 })
        );
        assert_eq!(
            validate(&[Instruction::statement(JMP | JA, 1), ret(RET_ALLOW)]),
            Err(Invalid::Jump { at: 0 })
        );
        assert_eq!(
            validate(&[Instruction::jump(JMP | JEQ | K, 0, 0, 1), ret(RET_ALLOW)]),
            Err(Invalid::Jump { at: 0 })
        );
        assert_eq!(validate(&[ret(RET_ALLOW), LD_NR]), Err(Invalid::NoReturn));
    }

    #[test]
    fn decodes_actions_and_applies_precedence() {
        assert_eq!(Action::from_result(RET_ERRNO | 38), Action::Errno(38));
        assert_eq!(Action::from_result(RET_TRAP | 1), Action::Trap(1));
        assert_eq!(Action::from_result(RET_ALLOW | 5), Action::Allow);
        assert_eq!(Action::from_result(0x1234_0000), Action::KillProcess);
        assert_eq!(combine(RET_ALLOW, RET_ERRNO | 1), RET_ERRNO | 1);
        assert_eq!(combine(RET_ERRNO | 1, RET_TRAP), RET_TRAP);
        assert_eq!(combine(RET_TRAP, RET_KILL_THREAD), RET_KILL_THREAD);
        assert_eq!(combine(RET_KILL_THREAD, RET_KILL_PROCESS), RET_KILL_PROCESS);
        assert_eq!(combine(RET_ERRNO | 1, RET_ERRNO | 2), RET_ERRNO | 1);
        assert_eq!(combine(RET_KILL_PROCESS, RET_ALLOW), RET_KILL_PROCESS);
    }

    #[test]
    fn decodes_sock_filter_bytes() {
        let mut bytes = [0u8; 8];
        bytes[0..2].copy_from_slice(&(JMP | JEQ | K).to_ne_bytes());
        bytes[2] = 3;
        bytes[3] = 4;
        bytes[4..8].copy_from_slice(&41u32.to_ne_bytes());
        assert_eq!(
            Instruction::from_bytes(bytes),
            Instruction::jump(JMP | JEQ | K, 41, 3, 4)
        );
    }
}
