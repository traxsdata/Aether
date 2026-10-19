// SPDX-License-Identifier: Apache-2.0
//! Kernel endpoint object storage.
//!
//! Object generation checks and interrupt masking live here; message and
//! capability validation live in the host-tested `aether-ipc` crate.

use crate::arch::x86_64::timer::InterruptGuard;
use aether_ipc::{CapHandle, CapTable, Endpoint, IpcError, ObjectId, ReceivedMessage, Rights};
use core::cell::UnsafeCell;

/// Maximum number of endpoint objects.
pub const MAX_ENDPOINTS: usize = 4;

struct Slot {
    alive: bool,
    generation: u16,
    endpoint: Endpoint,
}

impl Slot {
    const EMPTY: Self = Self {
        alive: false,
        generation: 0,
        endpoint: Endpoint::new(),
    };
}

struct Global<T>(UnsafeCell<T>);

// SAFETY: All access is serialized with `InterruptGuard`.
unsafe impl<T> Sync for Global<T> {}

static SLOTS: Global<[Slot; MAX_ENDPOINTS]> = Global(UnsafeCell::new([Slot::EMPTY; MAX_ENDPOINTS]));

fn slots() -> *mut Slot {
    SLOTS.0.get().cast::<Slot>()
}

/// Creates a new endpoint object and returns its generation-checked id.
pub fn create_endpoint() -> Option<ObjectId> {
    let _guard = InterruptGuard::enter();
    for index in 0..MAX_ENDPOINTS {
        // SAFETY: `index` is inside the endpoint array and interrupts are
        // masked for the duration of the update.
        let slot = unsafe { &mut *slots().add(index) };
        if !slot.alive {
            slot.alive = true;
            slot.generation = slot.generation.wrapping_add(1).max(1);
            slot.endpoint = Endpoint::new();
            return Some(ObjectId {
                index: index as u16,
                generation: slot.generation,
            });
        }
    }
    None
}

fn endpoint_mut(id: ObjectId) -> Result<&'static mut Endpoint, IpcError> {
    let index = usize::from(id.index);
    if index >= MAX_ENDPOINTS {
        return Err(IpcError::EndpointGone);
    }
    // SAFETY: Bounds checked. Callers hold the interrupt guard.
    let slot = unsafe { &mut *slots().add(index) };
    if !slot.alive || slot.generation != id.generation {
        return Err(IpcError::EndpointGone);
    }
    Ok(&mut slot.endpoint)
}

/// Sends a message through an endpoint capability.
///
/// `sender` is the kernel's authenticated task identity. A user-mode syscall
/// wrapper must derive it from the current task and must never copy it from a
/// user-supplied message field.
pub fn send(
    caps: &CapTable,
    endpoint_cap: CapHandle,
    sender: u16,
    payload: &[u8],
    transfer: Option<CapHandle>,
) -> Result<(), IpcError> {
    let (id, _) = caps.endpoint(endpoint_cap, Rights::SEND)?;
    let _guard = InterruptGuard::enter();
    endpoint_mut(id)?.send(id, caps, endpoint_cap, sender, payload, transfer)
}

/// Receives a message through an endpoint capability.
pub fn receive(caps: &mut CapTable, endpoint_cap: CapHandle) -> Result<ReceivedMessage, IpcError> {
    let (id, _) = caps.endpoint(endpoint_cap, Rights::RECEIVE)?;
    let _guard = InterruptGuard::enter();
    endpoint_mut(id)?.receive(id, caps, endpoint_cap)
}
