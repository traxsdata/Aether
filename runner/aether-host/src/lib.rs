//! aether-host - æther's own system calls for programs that run in its
//! Linux-compatible layer, plus a `Backend` the Rust runner draws through.
//!
//! `abi` and `sys` are `no_std`; the rest needs the `std` feature (default).
//! `open()` picks the backend at run time: æther when `AETHER=1` is set in
//! the environment (the kernel sets it), the headless one otherwise.

#![cfg_attr(not(feature = "std"), no_std)]

pub mod abi;
pub mod aether;
pub mod backend;
pub mod sys;

#[cfg(feature = "std")]
pub mod headless;
#[cfg(feature = "std")]
pub mod net;
#[cfg(feature = "std")]
pub mod png;

pub use aether::AetherBackend;
pub use backend::*;
#[cfg(feature = "std")]
pub use headless::{HeadlessBackend, Script};

/// An æther error: the positive E_* code.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Error(pub i32);

impl Error {
    pub fn name(self) -> &'static str {
        match self.0 {
            abi::E_PERM => "missing capability",
            abi::E_FAULT => "bad pointer",
            abi::E_INVAL => "invalid argument",
            abi::E_NOMEM => "out of memory",
            abi::E_NOENT => "not found",
            abi::E_LIMIT => "limit reached",
            abi::E_TIMEOUT => "timed out",
            abi::E_EXIST => "already exists",
            abi::E_NOTDIR => "not a folder",
            abi::E_ISDIR => "is a folder",
            abi::E_NOSPACE => "no space left",
            abi::E_IO => "I/O error or failed integrity check",
            abi::E_DENIED => "denied",
            abi::E_LOCKED => "storage locked",
            abi::E_NOTEMPTY => "folder not empty",
            abi::E_NET => "network error",
            abi::E_BUSY => "busy",
            abi::E_NOTSUP => "not supported",
            _ => "unknown error",
        }
    }
}

impl core::fmt::Display for Error {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(f, "{} (E{})", self.name(), self.0)
    }
}

#[cfg(feature = "std")]
impl std::error::Error for Error {}

/// True when running on æther (the kernel sets AETHER=1).
#[cfg(feature = "std")]
pub fn on_aether() -> bool {
    std::env::var_os("AETHER").is_some_and(|v| v == "1")
}

/// The backend for this process: æther's when `AETHER=1`, else headless
/// (configured from AETHER_SCRIPT, AETHER_DUMP_DIR and AETHER_PICK).
#[cfg(feature = "std")]
pub fn open() -> std::boxed::Box<dyn Backend + Send> {
    if on_aether() {
        std::boxed::Box::new(AetherBackend::new())
    } else {
        std::boxed::Box::new(HeadlessBackend::from_env())
    }
}
