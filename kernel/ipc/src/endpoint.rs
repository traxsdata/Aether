// SPDX-License-Identifier: Apache-2.0
//! Bounded endpoint message queues.
//!
//! Endpoint logic is pure: the caller owns the endpoint object and provides
//! the capability table. The kernel wraps this in a generation-checked object
//! table and masks interrupts while a queue is mutated.

use crate::capability::{CapError, CapHandle, CapTable, ObjectId, Rights, TransferCap};

/// Maximum queued messages per endpoint.
pub const QUEUE_LEN: usize = 4;
/// Maximum message payload.
pub const MAX_MESSAGE_BYTES: usize = 32;

/// Errors from endpoint operations.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum IpcError {
    /// Capability lookup failed.
    Capability(CapError),
    /// The endpoint queue is full.
    Full,
    /// The endpoint queue is empty.
    Empty,
    /// The endpoint object is no longer alive or its generation is stale.
    EndpointGone,
    /// The capability refers to a different endpoint object.
    WrongEndpoint,
    /// Payload exceeds [`MAX_MESSAGE_BYTES`].
    MessageTooLong,
}

impl From<CapError> for IpcError {
    fn from(error: CapError) -> Self {
        Self::Capability(error)
    }
}

#[derive(Clone, Copy)]
struct Transfer {
    cap: TransferCap,
}

#[derive(Clone, Copy)]
struct Message {
    sender: u16,
    len: u8,
    data: [u8; MAX_MESSAGE_BYTES],
    transfer: Option<Transfer>,
}

impl Message {
    const EMPTY: Self = Self {
        sender: 0,
        len: 0,
        data: [0; MAX_MESSAGE_BYTES],
        transfer: None,
    };
}

/// A received message.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ReceivedMessage {
    /// Sender task identifier.
    pub sender: u16,
    /// Number of valid bytes in `data`.
    pub len: usize,
    /// Payload bytes.
    pub data: [u8; MAX_MESSAGE_BYTES],
    /// Capability installed in the receiver's table, if one was transferred.
    pub transferred: Option<CapHandle>,
}

impl ReceivedMessage {
    /// Returns the payload as a slice.
    #[must_use]
    pub fn payload(&self) -> &[u8] {
        &self.data[..self.len]
    }
}

/// A bounded message queue.
pub struct Endpoint {
    head: u8,
    tail: u8,
    len: u8,
    queue: [Message; QUEUE_LEN],
}

impl Endpoint {
    /// Creates an empty endpoint.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            head: 0,
            tail: 0,
            len: 0,
            queue: [Message::EMPTY; QUEUE_LEN],
        }
    }

    /// Sends a bounded message.
    ///
    /// `endpoint_id` is this endpoint's object identity; `endpoint_cap` must
    /// refer to the same object. `sender` is the kernel-authenticated sender
    /// identity, never a value taken from an untrusted payload. `transfer`
    /// names a capability in `caps`; it must carry `TRANSFER` and is installed
    /// in the receiver's table when the message is received.
    pub fn send(
        &mut self,
        endpoint_id: ObjectId,
        caps: &CapTable,
        endpoint_cap: CapHandle,
        sender: u16,
        payload: &[u8],
        transfer: Option<CapHandle>,
    ) -> Result<(), IpcError> {
        if payload.len() > MAX_MESSAGE_BYTES {
            return Err(IpcError::MessageTooLong);
        }
        let (capability_endpoint, _) = caps.endpoint(endpoint_cap, Rights::SEND)?;
        if capability_endpoint != endpoint_id {
            return Err(IpcError::WrongEndpoint);
        }
        let transfer = match transfer {
            Some(handle) => Some(Transfer {
                cap: caps.capture_transfer(handle)?,
            }),
            None => None,
        };

        if usize::from(self.len) == QUEUE_LEN {
            return Err(IpcError::Full);
        }
        let slot = usize::from(self.tail);
        let mut message = Message {
            sender,
            len: payload.len() as u8,
            ..Message::EMPTY
        };
        message.data[..payload.len()].copy_from_slice(payload);
        message.transfer = transfer;
        self.queue[slot] = message;
        self.tail = ((slot + 1) % QUEUE_LEN) as u8;
        self.len += 1;
        Ok(())
    }

    /// Receives a message, installing any transferred capability in `caps`.
    ///
    /// The capability install happens before the queue head advances. If the
    /// receiver's table is full, the message stays queued and can be received
    /// after the receiver makes room; a resource-exhaustion failure must not
    /// destroy a message or its capability.
    pub fn receive(
        &mut self,
        endpoint_id: ObjectId,
        caps: &mut CapTable,
        endpoint_cap: CapHandle,
    ) -> Result<ReceivedMessage, IpcError> {
        let (capability_endpoint, _) = caps.endpoint(endpoint_cap, Rights::RECEIVE)?;
        if capability_endpoint != endpoint_id {
            return Err(IpcError::WrongEndpoint);
        }
        if self.len == 0 {
            return Err(IpcError::Empty);
        }
        let slot = usize::from(self.head);
        let message = self.queue[slot];
        let transferred = match message.transfer {
            Some(transfer) => Some(caps.insert_transfer(transfer.cap)?),
            None => None,
        };
        self.head = ((slot + 1) % QUEUE_LEN) as u8;
        self.len -= 1;

        Ok(ReceivedMessage {
            sender: message.sender,
            len: usize::from(message.len),
            data: message.data,
            transferred,
        })
    }

    /// Returns `true` if the queue is empty.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.len == 0
    }
}

impl Default for Endpoint {
    fn default() -> Self {
        Self::new()
    }
}

/// Endpoint object identifiers use the same generation-checked object ID as
/// other capability-addressed objects.
pub type EndpointId = ObjectId;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::capability::{MAX_CAPS, ObjectType};

    fn cap_for(endpoint: ObjectId, rights: Rights) -> (CapTable, CapHandle) {
        let mut table = CapTable::new();
        let handle = table
            .grant(endpoint, ObjectType::Endpoint, rights)
            .expect("slot");
        (table, handle)
    }

    fn id(index: u16) -> ObjectId {
        ObjectId {
            index,
            generation: 1,
        }
    }

    #[test]
    fn sends_and_receives_bounded_messages() {
        let mut endpoint = Endpoint::new();
        let (sender_caps, send_cap) = cap_for(id(0), Rights::SEND);
        let (mut receiver_caps, recv_cap) = cap_for(id(0), Rights::RECEIVE);

        endpoint
            .send(id(0), &sender_caps, send_cap, 2, b"hello", None)
            .expect("send");
        let message = endpoint
            .receive(id(0), &mut receiver_caps, recv_cap)
            .expect("receive");
        assert_eq!(message.sender, 2);
        assert_eq!(message.payload(), b"hello");
        assert!(message.transferred.is_none());
        assert_eq!(
            endpoint.receive(id(0), &mut receiver_caps, recv_cap),
            Err(IpcError::Empty)
        );
    }

    #[test]
    fn rejects_missing_rights_and_oversized_payloads() {
        let mut endpoint = Endpoint::new();
        let (caps, receive_cap) = cap_for(id(0), Rights::RECEIVE);
        assert_eq!(
            endpoint.send(id(0), &caps, receive_cap, 1, b"nope", None),
            Err(IpcError::Capability(CapError::InsufficientRights))
        );

        let (caps, send_cap) = cap_for(id(0), Rights::SEND);
        assert_eq!(
            endpoint.send(id(0), &caps, send_cap, 1, &[0; MAX_MESSAGE_BYTES + 1], None),
            Err(IpcError::MessageTooLong)
        );
    }

    #[test]
    fn rejects_capability_for_another_endpoint() {
        let mut endpoint = Endpoint::new();
        let (caps, send_cap) = cap_for(id(1), Rights::SEND);
        assert_eq!(
            endpoint.send(id(0), &caps, send_cap, 1, b"wrong endpoint", None),
            Err(IpcError::WrongEndpoint)
        );
    }

    #[test]
    fn rejects_forged_capability_handles() {
        let mut endpoint = Endpoint::new();
        let mut caps = CapTable::new();
        let forged = CapHandle {
            index: 15,
            generation: 1,
        };
        assert_eq!(
            endpoint.receive(id(0), &mut caps, forged),
            Err(IpcError::Capability(CapError::InvalidHandle))
        );
    }

    #[test]
    fn enforces_queue_capacity() {
        let mut endpoint = Endpoint::new();
        let (caps, send_cap) = cap_for(id(0), Rights::SEND);
        for _ in 0..QUEUE_LEN {
            endpoint
                .send(id(0), &caps, send_cap, 1, b"x", None)
                .expect("send");
        }
        assert_eq!(
            endpoint.send(id(0), &caps, send_cap, 1, b"x", None),
            Err(IpcError::Full)
        );
    }

    #[test]
    fn transfers_a_capability_to_the_receiver() {
        let mut first = Endpoint::new();
        let mut second = Endpoint::new();
        let (mut receiver_caps, first_recv) = cap_for(id(0), Rights::RECEIVE);
        let (mut sender_caps, first_send) = cap_for(id(0), Rights::SEND | Rights::TRANSFER);
        let second_send = sender_caps
            .grant(id(1), ObjectType::Endpoint, Rights::SEND | Rights::TRANSFER)
            .expect("slot");

        // A capability without TRANSFER cannot be passed through IPC.
        let send_only = sender_caps
            .grant(id(2), ObjectType::Endpoint, Rights::SEND)
            .expect("slot");
        assert_eq!(
            first.send(id(0), &sender_caps, first_send, 2, b"bad", Some(send_only)),
            Err(IpcError::Capability(CapError::InsufficientRights))
        );

        first
            .send(
                id(0),
                &sender_caps,
                first_send,
                2,
                b"cap",
                Some(second_send),
            )
            .expect("send capability");
        let message = first
            .receive(id(0), &mut receiver_caps, first_recv)
            .expect("receive");
        let transferred = message.transferred.expect("transferred capability");

        // A capability for endpoint B must not authorize an operation on
        // endpoint A, even though the handle belongs to the receiver's table.
        assert_eq!(
            first.send(
                id(0),
                &receiver_caps,
                transferred,
                1,
                b"wrong endpoint",
                None
            ),
            Err(IpcError::WrongEndpoint)
        );

        // The receiver can use the transferred send capability on endpoint B.
        second
            .send(id(1), &receiver_caps, transferred, 1, b"via transfer", None)
            .expect("send with transferred capability");
        let (mut second_receiver, second_recv) = cap_for(id(1), Rights::RECEIVE);
        let received = second
            .receive(id(1), &mut second_receiver, second_recv)
            .expect("receive");
        assert_eq!(received.payload(), b"via transfer");
        assert_eq!(received.sender, 1);
    }

    #[test]
    fn table_full_does_not_consume_message_or_transfer() {
        let mut endpoint = Endpoint::new();
        let (mut sender_caps, send_cap) = cap_for(id(0), Rights::SEND | Rights::TRANSFER);
        let transfer = sender_caps
            .grant(id(1), ObjectType::Endpoint, Rights::SEND | Rights::TRANSFER)
            .expect("slot");
        let (mut receiver_caps, recv_cap) = cap_for(id(0), Rights::RECEIVE);
        for _ in 1..MAX_CAPS {
            receiver_caps
                .grant(id(2), ObjectType::Endpoint, Rights::SEND)
                .expect("slot");
        }

        endpoint
            .send(id(0), &sender_caps, send_cap, 2, b"cap", Some(transfer))
            .expect("send");
        assert_eq!(
            endpoint.receive(id(0), &mut receiver_caps, recv_cap),
            Err(IpcError::Capability(CapError::TableFull))
        );

        // The failed receive left the message at the head of the queue. A
        // receiver with capacity can still consume it and its capability.
        let (mut fresh_caps, fresh_recv) = cap_for(id(0), Rights::RECEIVE);
        let message = endpoint
            .receive(id(0), &mut fresh_caps, fresh_recv)
            .expect("queued message survives");
        assert_eq!(message.payload(), b"cap");
        assert!(message.transferred.is_some());
    }
}
