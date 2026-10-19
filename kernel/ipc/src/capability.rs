// SPDX-License-Identifier: Apache-2.0
//! Capability tables.
//!
//! A capability is a table slot plus a generation counter. The index alone is
//! not authority: a forged index, a stale generation, an empty slot, the wrong
//! object type, or missing rights all fail validation. Object generations are
//! carried separately so a capability cannot outlive an object whose slot has
//! been recycled.

/// Number of capability slots per task.
pub const MAX_CAPS: usize = 16;

/// Identifier of a kernel object.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ObjectId {
    /// Object slot.
    pub index: u16,
    /// Object generation, to reject references to recycled slots.
    pub generation: u16,
}

/// A handle to a capability table slot.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CapHandle {
    /// Slot index.
    pub index: u8,
    /// Generation of the slot when the handle was issued.
    pub generation: u16,
}

/// Object type stored in a capability.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum ObjectType {
    /// Empty slot.
    Empty = 0,
    /// Message endpoint.
    Endpoint = 1,
    /// Physical frame object (future memory sharing).
    Frame = 2,
    /// Open file object owned by the file service.
    File = 3,
}

/// Capability rights.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Rights(u8);

impl Rights {
    /// Send messages to an endpoint.
    pub const SEND: Self = Self(1 << 0);
    /// Receive messages from an endpoint.
    pub const RECEIVE: Self = Self(1 << 1);
    /// Transfer the capability through IPC.
    pub const TRANSFER: Self = Self(1 << 2);
    /// Read the referenced object.
    pub const READ: Self = Self(1 << 3);
    /// Write the referenced object.
    pub const WRITE: Self = Self(1 << 4);

    /// No rights.
    #[must_use]
    pub const fn empty() -> Self {
        Self(0)
    }

    /// Raw bits.
    #[must_use]
    pub const fn bits(self) -> u8 {
        self.0
    }

    /// Returns `true` if all rights in `other` are present.
    #[must_use]
    pub const fn contains(self, other: Self) -> bool {
        self.0 & other.0 == other.0
    }

    /// Removes `other` from this set.
    #[must_use]
    pub const fn without(self, other: Self) -> Self {
        Self(self.0 & !other.0)
    }

    /// Combines rights.
    #[must_use]
    pub const fn union(self, other: Self) -> Self {
        Self(self.0 | other.0)
    }
}

impl core::ops::BitOr for Rights {
    type Output = Self;

    fn bitor(self, rhs: Self) -> Self::Output {
        self.union(rhs)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Capability {
    kind: ObjectType,
    rights: Rights,
    object_index: u16,
    object_generation: u16,
    slot_generation: u16,
}

impl Capability {
    const EMPTY: Self = Self {
        kind: ObjectType::Empty,
        rights: Rights::empty(),
        object_index: 0,
        object_generation: 0,
        slot_generation: 0,
    };

    const fn is_empty(self) -> bool {
        matches!(self.kind, ObjectType::Empty)
    }
}

/// Errors from capability lookup and insertion.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CapError {
    /// Slot index, generation, or object generation is invalid.
    InvalidHandle,
    /// The capability does not carry every required right.
    InsufficientRights,
    /// No empty slot is available.
    TableFull,
    /// The capability refers to a different object type.
    WrongObjectType,
}

/// Per-task capability table.
pub struct CapTable {
    slots: [Capability; MAX_CAPS],
}

impl CapTable {
    /// Creates an empty table.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            slots: [Capability::EMPTY; MAX_CAPS],
        }
    }

    /// Grants a capability for `object` with `rights`.
    pub fn grant(
        &mut self,
        object: ObjectId,
        kind: ObjectType,
        rights: Rights,
    ) -> Result<CapHandle, CapError> {
        let index = self
            .slots
            .iter()
            .position(|slot| slot.is_empty())
            .ok_or(CapError::TableFull)?;
        let next_generation = self.slots[index].slot_generation.wrapping_add(1).max(1);
        self.slots[index] = Capability {
            kind,
            rights,
            object_index: object.index,
            object_generation: object.generation,
            slot_generation: next_generation,
        };
        Ok(CapHandle {
            index: index as u8,
            generation: next_generation,
        })
    }

    /// Validates a handle and required rights, returning the capability.
    fn check(&self, handle: CapHandle, required: Rights) -> Result<Capability, CapError> {
        let cap = self
            .slots
            .get(usize::from(handle.index))
            .copied()
            .ok_or(CapError::InvalidHandle)?;
        if cap.is_empty() || cap.slot_generation != handle.generation {
            return Err(CapError::InvalidHandle);
        }
        if !cap.rights.contains(required) {
            return Err(CapError::InsufficientRights);
        }
        Ok(cap)
    }

    /// Returns the endpoint referenced by a send/receive handle.
    pub fn endpoint(
        &self,
        handle: CapHandle,
        required: Rights,
    ) -> Result<(ObjectId, Rights), CapError> {
        let cap = self.check(handle, required)?;
        if !matches!(cap.kind, ObjectType::Endpoint) {
            return Err(CapError::WrongObjectType);
        }
        Ok((
            ObjectId {
                index: cap.object_index,
                generation: cap.object_generation,
            },
            cap.rights,
        ))
    }

    /// Returns the file object referenced by a handle.
    pub fn file(&self, handle: CapHandle, required: Rights) -> Result<ObjectId, CapError> {
        let cap = self.check(handle, required)?;
        if !matches!(cap.kind, ObjectType::File) {
            return Err(CapError::WrongObjectType);
        }
        Ok(ObjectId {
            index: cap.object_index,
            generation: cap.object_generation,
        })
    }

    /// Revokes a capability. The slot generation advances so a stale handle
    /// cannot name a future capability that reuses the slot.
    pub fn remove(&mut self, handle: CapHandle) -> Result<(), CapError> {
        let slot = self
            .slots
            .get_mut(usize::from(handle.index))
            .ok_or(CapError::InvalidHandle)?;
        if slot.is_empty() || slot.slot_generation != handle.generation {
            return Err(CapError::InvalidHandle);
        }
        *slot = Capability {
            slot_generation: slot.slot_generation.wrapping_add(1).max(1),
            ..Capability::EMPTY
        };
        Ok(())
    }

    /// Captures a capability for transfer.
    ///
    /// The source must carry [`Rights::TRANSFER`]. The returned descriptor is
    /// inserted into the receiver's table when the message is received.
    pub fn capture_transfer(&self, handle: CapHandle) -> Result<TransferCap, CapError> {
        let cap = self.check(handle, Rights::TRANSFER)?;
        Ok(TransferCap {
            kind: cap.kind,
            rights: cap.rights.without(Rights::TRANSFER),
            object_index: cap.object_index,
            object_generation: cap.object_generation,
        })
    }

    /// Inserts a transferred capability.
    pub fn insert_transfer(&mut self, cap: TransferCap) -> Result<CapHandle, CapError> {
        let index = self
            .slots
            .iter()
            .position(|slot| slot.is_empty())
            .ok_or(CapError::TableFull)?;
        let next_generation = self.slots[index].slot_generation.wrapping_add(1).max(1);
        self.slots[index] = Capability {
            kind: cap.kind,
            rights: cap.rights,
            object_index: cap.object_index,
            object_generation: cap.object_generation,
            slot_generation: next_generation,
        };
        Ok(CapHandle {
            index: index as u8,
            generation: next_generation,
        })
    }
}

impl Default for CapTable {
    fn default() -> Self {
        Self::new()
    }
}

/// Capability descriptor carried inside an IPC message.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TransferCap {
    kind: ObjectType,
    rights: Rights,
    object_index: u16,
    object_generation: u16,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn object(index: u16) -> ObjectId {
        ObjectId {
            index,
            generation: 1,
        }
    }

    #[test]
    fn grants_and_validates_rights() {
        let mut table = CapTable::new();
        let handle = table
            .grant(
                object(0),
                ObjectType::Endpoint,
                Rights::SEND | Rights::TRANSFER,
            )
            .expect("slot");
        assert!(table.check(handle, Rights::SEND).is_ok());
        assert_eq!(
            table.check(handle, Rights::RECEIVE),
            Err(CapError::InsufficientRights)
        );
        assert_eq!(
            table
                .check(handle, Rights::SEND | Rights::TRANSFER)
                .unwrap()
                .object_index,
            0
        );
    }

    #[test]
    fn rejects_forged_and_stale_handles() {
        let mut table = CapTable::new();
        let handle = table
            .grant(object(0), ObjectType::Endpoint, Rights::SEND)
            .expect("slot");
        assert_eq!(
            table.check(
                CapHandle {
                    index: 15,
                    generation: handle.generation
                },
                Rights::SEND
            ),
            Err(CapError::InvalidHandle)
        );
        assert_eq!(
            table.check(
                CapHandle {
                    index: handle.index,
                    generation: handle.generation.wrapping_add(1)
                },
                Rights::SEND
            ),
            Err(CapError::InvalidHandle)
        );
    }

    #[test]
    fn table_full_is_reported() {
        let mut table = CapTable::new();
        for index in 0..MAX_CAPS {
            table
                .grant(object(index as u16), ObjectType::Endpoint, Rights::SEND)
                .expect("slot");
        }
        assert_eq!(
            table.grant(object(0), ObjectType::Endpoint, Rights::SEND),
            Err(CapError::TableFull)
        );
    }

    #[test]
    fn transfer_requires_transfer_right_and_removes_it() {
        let mut table = CapTable::new();
        let send_only = table
            .grant(object(0), ObjectType::Endpoint, Rights::SEND)
            .expect("slot");
        assert_eq!(
            table.capture_transfer(send_only),
            Err(CapError::InsufficientRights)
        );

        let transferable = table
            .grant(
                object(1),
                ObjectType::Endpoint,
                Rights::SEND | Rights::TRANSFER,
            )
            .expect("slot");
        let captured = table.capture_transfer(transferable).expect("transfer");
        let mut receiver = CapTable::new();
        let new_handle = receiver.insert_transfer(captured).expect("slot");
        assert!(receiver.check(new_handle, Rights::SEND).is_ok());
        assert_eq!(
            receiver.check(new_handle, Rights::TRANSFER),
            Err(CapError::InsufficientRights)
        );
    }

    #[test]
    fn file_capabilities_check_rights_and_type() {
        let mut table = CapTable::new();
        let handle = table
            .grant(
                ObjectId {
                    index: 4,
                    generation: 9,
                },
                ObjectType::File,
                Rights::READ | Rights::WRITE,
            )
            .expect("slot");
        assert_eq!(
            table.file(handle, Rights::READ),
            Ok(ObjectId {
                index: 4,
                generation: 9
            })
        );
        assert_eq!(
            table.file(handle, Rights::TRANSFER),
            Err(CapError::InsufficientRights)
        );

        let endpoint = table
            .grant(object(0), ObjectType::Endpoint, Rights::SEND)
            .expect("slot");
        assert_eq!(
            table.file(endpoint, Rights::SEND),
            Err(CapError::WrongObjectType)
        );
    }

    #[test]
    fn removal_invalidates_the_handle_and_preserves_the_generation_boundary() {
        let mut table = CapTable::new();
        let handle = table
            .grant(object(0), ObjectType::File, Rights::READ)
            .expect("slot");
        table.remove(handle).expect("remove");
        assert_eq!(
            table.file(handle, Rights::READ),
            Err(CapError::InvalidHandle)
        );
        let replacement = table
            .grant(object(1), ObjectType::File, Rights::READ)
            .expect("slot");
        assert_eq!(replacement.index, handle.index);
        assert_ne!(replacement.generation, handle.generation);
        assert_eq!(
            table.file(handle, Rights::READ),
            Err(CapError::InvalidHandle)
        );
    }

    #[test]
    fn wrong_object_type_is_rejected() {
        let mut table = CapTable::new();
        let handle = table
            .grant(object(0), ObjectType::Frame, Rights::SEND)
            .expect("slot");
        assert_eq!(
            table.endpoint(handle, Rights::SEND),
            Err(CapError::WrongObjectType)
        );
    }
}
