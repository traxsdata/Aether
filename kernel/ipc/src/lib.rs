// SPDX-License-Identifier: Apache-2.0
//! Host-testable capability tables and bounded IPC endpoints.
//!
//! This crate contains the policy-free object logic. The kernel supplies
//! object storage, generation allocation, and interrupt masking; it must not
//! duplicate the validation rules implemented here.

#![no_std]

pub mod capability;
pub mod endpoint;

pub use capability::{CapError, CapHandle, CapTable, ObjectId, ObjectType, Rights};
pub use endpoint::{Endpoint, IpcError, MAX_MESSAGE_BYTES, QUEUE_LEN, ReceivedMessage};
