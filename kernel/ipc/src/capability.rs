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

    /// Returns the rights that are meaningful for `kind`.
    ///
    /// Minting a capability with a right outside this set is rejected so a
    /// caller cannot mint a self-authorizing capability (spec rule: rights are
    /// limited to operations appropriate for the object type).
    #[must_use]
    pub const fn for_object(kind: ObjectType) -> Self {
        match kind {
            ObjectType::Empty => Self(0),
            ObjectType::Endpoint => Self::SEND.union(Self::RECEIVE).union(Self::TRANSFER),
            // Frame objects have no exposed operations yet; no capability for
            // them can be minted until their sharing semantics are defined.
            ObjectType::Frame => Self(0),
            ObjectType::File => Self::READ.union(Self::WRITE),
        }
    }

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
    /// The requested rights are not valid for the object type.
    InvalidRights,
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
    ///
    /// The rights must be valid for the object type; see
    /// [`Rights::for_object`]. Granting a self-authorizing right set is
    /// rejected rather than silently attenuated. A `File` capability may
    /// carry the empty set for a create-only open, which still needs a
    /// closeable handle; other object kinds require at least one right.
    pub fn grant(
        &mut self,
        object: ObjectId,
        kind: ObjectType,
        rights: Rights,
    ) -> Result<CapHandle, CapError> {
        if rights.bits() & !Rights::for_object(kind).bits() != 0 {
            return Err(CapError::InvalidRights);
        }
        // A create-only file open has no read or write right yet still needs a
        // closeable handle, so File accepts the empty set. Other object types
        // must name at least one operation they can perform.
        if rights.bits() == 0 && !matches!(kind, ObjectType::File) {
            return Err(CapError::InvalidRights);
        }
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

    /// Validates a handle and returns the capability it denotes.
    fn lookup(&self, handle: CapHandle) -> Result<Capability, CapError> {
        let cap = self
            .slots
            .get(usize::from(handle.index))
            .copied()
            .ok_or(CapError::InvalidHandle)?;
        if cap.is_empty() || cap.slot_generation != handle.generation {
            return Err(CapError::InvalidHandle);
        }
        Ok(cap)
    }

    /// Validates a handle and its rights, returning the capability.
    fn check(&self, handle: CapHandle, required: Rights) -> Result<Capability, CapError> {
        let cap = self.lookup(handle)?;
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
        let cap = self.lookup(handle)?;
        if !matches!(cap.kind, ObjectType::Endpoint) {
            return Err(CapError::WrongObjectType);
        }
        if !cap.rights.contains(required) {
            return Err(CapError::InsufficientRights);
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
        let cap = self.lookup(handle)?;
        if !matches!(cap.kind, ObjectType::File) {
            return Err(CapError::WrongObjectType);
        }
        if !cap.rights.contains(required) {
            return Err(CapError::InsufficientRights);
        }
        Ok(ObjectId {
            index: cap.object_index,
            generation: cap.object_generation,
        })
    }

    /// Returns the number of granted capabilities.
    #[must_use]
    pub fn len(&self) -> usize {
        self.slots.iter().filter(|slot| !slot.is_empty()).count()
    }

    /// Returns `true` when no capability has been granted.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Returns `true` if at least one capability slot is free.
    ///
    /// Callers that must create an object before they can mint its capability
    /// use this to fail before allocating the object, so a full table cannot
    /// leak a kernel object that no handle can name.
    #[must_use]
    pub const fn has_free_slot(&self) -> bool {
        let mut index = 0usize;
        while index < MAX_CAPS {
            if self.slots[index].is_empty() {
                return true;
            }
            index += 1;
        }
        false
    }

    /// Returns the number of capabilities that name a `File` object.
    ///
    /// The file service uses this as the caller's current open-handle count
    /// when enforcing per-application limits.
    #[must_use]
    pub const fn file_count(&self) -> usize {
        let mut count = 0usize;
        let mut index = 0usize;
        while index < MAX_CAPS {
            let slot = &self.slots[index];
            if !slot.is_empty() && matches!(slot.kind, ObjectType::File) {
                count += 1;
            }
            index += 1;
        }
        count
    }

    /// Snapshots the handles of all held `File` capabilities.
    ///
    /// The returned array is zero-padded; only the first `count` entries are
    /// valid. Callers must copy this snapshot before mutating the table, so a
    /// release loop cannot iterate a table it is shrinking.
    #[must_use]
    pub fn file_handles(&self) -> ([CapHandle; MAX_CAPS], usize) {
        let mut out = [CapHandle {
            index: 0,
            generation: 0,
        }; MAX_CAPS];
        let mut count = 0usize;
        let mut index = 0usize;
        while index < MAX_CAPS {
            let slot = &self.slots[index];
            if !slot.is_empty() && matches!(slot.kind, ObjectType::File) {
                out[count] = CapHandle {
                    index: index as u8,
                    generation: slot.slot_generation,
                };
                count += 1;
            }
            index += 1;
        }
        (out, count)
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
    ///
    /// As with [`CapTable::grant`], the descriptor's rights must be valid for
    /// its object type. A transferred capability can therefore never carry
    /// more authority than the minting path allows.
    pub fn insert_transfer(&mut self, cap: TransferCap) -> Result<CapHandle, CapError> {
        if cap.rights.bits() & !Rights::for_object(cap.kind).bits() != 0 {
            return Err(CapError::InvalidRights);
        }
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
        let endpoint = table
            .grant(object(0), ObjectType::Endpoint, Rights::SEND)
            .expect("slot");
        assert_eq!(
            table.endpoint(endpoint, Rights::SEND),
            Ok((object(0), Rights::SEND))
        );
        assert_eq!(
            table.file(endpoint, Rights::READ),
            Err(CapError::WrongObjectType)
        );
    }

    #[test]
    fn rights_are_limited_to_the_object_type() {
        let mut table = CapTable::new();
        // Endpoint rights on a file, file rights on an endpoint, and
        // meaningless rights on a Frame object are all refused at mint time
        // instead of silently attenuated.
        assert_eq!(
            table.grant(object(0), ObjectType::File, Rights::SEND),
            Err(CapError::InvalidRights)
        );
        assert_eq!(
            table.grant(object(0), ObjectType::Endpoint, Rights::READ),
            Err(CapError::InvalidRights)
        );
        assert_eq!(
            table.grant(object(0), ObjectType::Frame, Rights::SEND),
            Err(CapError::InvalidRights)
        );
        assert_eq!(
            table.grant(object(0), ObjectType::Frame, Rights::empty()),
            Err(CapError::InvalidRights)
        );

        // Create-only opens return an object with no read/write right; the
        // handle must still be mintable so it can be closed.
        let create_only = table
            .grant(object(0), ObjectType::File, Rights::empty())
            .expect("create-only slot");
        assert_eq!(table.file(create_only, Rights::empty()), Ok(object(0)));

        let file = table
            .grant(object(1), ObjectType::File, Rights::READ | Rights::WRITE)
            .expect("slot");
        assert!(table.file(file, Rights::READ).is_ok());

        let endpoint = table
            .grant(
                object(2),
                ObjectType::Endpoint,
                Rights::SEND | Rights::RECEIVE | Rights::TRANSFER,
            )
            .expect("slot");
        assert!(table.endpoint(endpoint, Rights::TRANSFER).is_ok());
    }

    #[test]
    fn transferred_rights_stay_valid_for_the_object_type() {
        let mut sender = CapTable::new();
        let source = sender
            .grant(
                object(0),
                ObjectType::Endpoint,
                Rights::SEND | Rights::RECEIVE | Rights::TRANSFER,
            )
            .expect("slot");
        let cap = match sender.check(source, Rights::TRANSFER) {
            Ok(cap) => cap,
            Err(_) => unreachable!("transfer right"),
        };
        let valid = TransferCap {
            kind: cap.kind,
            rights: cap.rights,
            object_index: cap.object_index,
            object_generation: cap.object_generation,
        };
        let mut receiver = CapTable::new();
        assert!(receiver.insert_transfer(valid).is_ok());

        // A transferred descriptor cannot carry rights the minting rules
        // forbid for the object type, so a malformed relay cannot smuggle
        // authority through IPC.
        let smuggled = TransferCap {
            kind: ObjectType::Endpoint,
            rights: Rights::READ,
            object_index: 0,
            object_generation: 1,
        };
        let mut other = CapTable::new();
        assert_eq!(
            other.insert_transfer(smuggled),
            Err(CapError::InvalidRights)
        );
    }

    #[test]
    fn free_slot_check_matches_table_capacity() {
        let mut table = CapTable::new();
        assert!(table.has_free_slot());
        let mut first = None;
        for index in 0..MAX_CAPS {
            let handle = table
                .grant(object(index as u16), ObjectType::Endpoint, Rights::SEND)
                .expect("slot");
            if index == 0 {
                first = Some(handle);
            }
        }
        assert!(!table.has_free_slot());
        assert_eq!(
            table.grant(object(0), ObjectType::Endpoint, Rights::SEND),
            Err(CapError::TableFull)
        );
        assert!(table.remove(first.expect("first handle")).is_ok());
        assert!(table.has_free_slot());
    }

    #[test]
    fn file_snapshot_and_count_track_handles() {
        let mut table = CapTable::new();
        assert_eq!(table.file_count(), 0);
        let first = table
            .grant(object(0), ObjectType::File, Rights::READ)
            .expect("slot");
        let endpoint = table
            .grant(object(0), ObjectType::Endpoint, Rights::SEND)
            .expect("slot");
        let second = table
            .grant(object(1), ObjectType::File, Rights::WRITE)
            .expect("slot");
        assert_eq!(table.file_count(), 2);

        let (handles, count) = table.file_handles();
        assert_eq!(count, 2);
        assert!(handles.iter().take(count).any(|h| *h == first));
        assert!(handles.iter().take(count).any(|h| *h == second));
        assert!(!handles[..count].contains(&endpoint));
    }
}
