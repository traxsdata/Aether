// SPDX-License-Identifier: Apache-2.0
//! Kernel endpoint object storage.
//!
//! Object generation checks and interrupt masking live here; message and
//! capability validation live in the host-tested `aether-ipc` crate.

use crate::arch::x86_64::timer::InterruptGuard;
use aether_ipc::{CapHandle, CapTable, Endpoint, IpcError, ObjectId, ReceivedMessage, Rights};
use core::cell::UnsafeCell;

/// Maximum number of endpoint objects.
pub const MAX_ENDPOINTS: usize = 8;

struct Slot {
    alive: bool,
    generation: u16,
    owner: u16,
    endpoint: Endpoint,
}

impl Slot {
    const EMPTY: Self = Self {
        alive: false,
        generation: 0,
        owner: 0,
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
///
/// `owner` is the kernel task id that may destroy the object. The owner is
/// never taken from a user-supplied field.
pub fn create_endpoint(owner: u16) -> Option<ObjectId> {
    let _guard = InterruptGuard::enter();
    for index in 0..MAX_ENDPOINTS {
        // SAFETY: `index` is inside the endpoint array and interrupts are
        // masked for the duration of the update.
        let slot = unsafe { &mut *slots().add(index) };
        if !slot.alive {
            slot.alive = true;
            slot.generation = slot.generation.wrapping_add(1).max(1);
            slot.owner = owner;
            slot.endpoint = Endpoint::new();
            return Some(ObjectId {
                index: index as u16,
                generation: slot.generation,
            });
        }
    }
    None
}

/// Destroys an endpoint object owned by `owner`.
///
/// Destruction advances the slot generation and clears the queue. Every
/// outstanding capability for the object, in every task, is now stale:
/// validating it reports [`IpcError::EndpointGone`]. The capability table of
/// the caller is cleaned by [`close`]; other tasks keep stale slots until they
/// attempt to use them or exit.
pub fn destroy(id: ObjectId, owner: u16) -> Result<(), IpcError> {
    let _guard = InterruptGuard::enter();
    let index = usize::from(id.index);
    if index >= MAX_ENDPOINTS {
        return Err(IpcError::EndpointGone);
    }
    // SAFETY: Bounds checked and interrupts are masked.
    let slot = unsafe { &mut *slots().add(index) };
    if !slot.alive || slot.generation != id.generation {
        return Err(IpcError::EndpointGone);
    }
    if slot.owner != owner {
        return Err(IpcError::Capability(
            aether_ipc::CapError::InsufficientRights,
        ));
    }
    slot.alive = false;
    slot.owner = 0;
    slot.generation = slot.generation.wrapping_add(1).max(1);
    slot.endpoint = Endpoint::new();
    Ok(())
}

/// Closes one endpoint capability, revoking every alias in the caller's table.
///
/// A capability for an endpoint owned by another task is only dropped from the
/// caller's table; the object itself remains until its owner destroys it or
/// exits. Removing every alias in one operation is the documented revocation
/// rule: a closed endpoint never leaves a usable alias behind in that table.
pub fn close(caps: &mut CapTable, endpoint_cap: CapHandle, task: u16) -> Result<(), IpcError> {
    let (id, _) = caps.endpoint(endpoint_cap, Rights::empty())?;
    caps.revoke_object(id);
    let owned = {
        let _guard = InterruptGuard::enter();
        let index = usize::from(id.index);
        if index >= MAX_ENDPOINTS {
            return Err(IpcError::EndpointGone);
        }
        // SAFETY: Bounds checked and interrupts are masked.
        let slot = unsafe { &*slots().add(index) };
        slot.alive && slot.generation == id.generation && slot.owner == task
    };
    if owned {
        destroy(id, task)?;
    }
    Ok(())
}

/// Releases every endpoint capability a retiring task holds.
///
/// Endpoints owned by the task are destroyed, which invalidates any aliases
/// other tasks received through capability transfer. The function returns
/// `(revoked handles, destroyed objects)`.
pub fn close_task_endpoints(caps: &mut CapTable, task: u16) -> (usize, usize) {
    let (handles, count) = caps.handles_of_type(aether_ipc::ObjectType::Endpoint);
    let mut revoked = 0usize;
    let mut destroyed = 0usize;
    for handle in handles.iter().take(count) {
        if let Ok((id, _)) = caps.endpoint(*handle, Rights::empty()) {
            revoked += caps.revoke_object(id);
            if destroy(id, task).is_ok() {
                destroyed += 1;
            }
        }
    }
    (revoked, destroyed)
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
