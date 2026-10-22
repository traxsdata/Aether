// SPDX-License-Identifier: Apache-2.0
//! Network policy that does not touch hardware.
//!
//! The kernel owns the NIC, the TCP/IP stack, and the socket table. The
//! decisions that guard them are pure functions kept here so they can be
//! tested on the host against hostile input:
//!
//! - [`addr`] decodes and encodes Linux `sockaddr_in`/`sockaddr_in6` and says
//!   which remote addresses an application may reach.
//! - [`firewall`] decides, per received Ethernet frame, whether it may reach
//!   the stack at all. Inbound is default-deny.
//! - [`consent`] is the codec for the per-application network decisions the
//!   user made.

#![no_std]
#![forbid(unsafe_code)]

pub mod addr;
pub mod consent;
pub mod firewall;

#[cfg(test)]
mod tests;
