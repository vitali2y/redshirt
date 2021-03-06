// Copyright (C) 2019  Pierre Krieger
//
// This program is free software: you can redistribute it and/or modify
// it under the terms of the GNU General Public License as published by
// the Free Software Foundation, either version 3 of the License, or
// (at your option) any later version.
//
// This program is distributed in the hope that it will be useful,
// but WITHOUT ANY WARRANTY; without even the implied warranty of
// MERCHANTABILITY or FITNESS FOR A PARTICULAR PURPOSE.  See the
// GNU General Public License for more details.
//
// You should have received a copy of the GNU General Public License
// along with this program.  If not, see <https://www.gnu.org/licenses/>.

use crate::{MessageId, Pid};

use alloc::vec::Vec;
use parity_scale_codec::{Decode, Encode};

#[link(wasm_import_module = "redshirt")]
extern "C" {
    /// Asks for the next message.
    ///
    /// The `to_poll` parameter must be a list (whose length is `to_poll_len`) of messages to poll.
    /// Entries in this list equal to `0` are ignored. Entries equal to `1` are special and mean
    /// "a message received on an interface or a process destroyed message". If a message is
    /// successfully pulled, the corresponding entry in `to_poll` is set to `0`.
    ///
    /// If `block` is true, then this function puts the thread to sleep until a message is
    /// available. If `block` is false, then this function returns as soon as possible.
    ///
    /// If the function returns 0, then there is no message available and nothing has been written.
    /// This function never returns 0 if `block` is `true`.
    /// If the function returns a value larger than `out_len`, then a message is available whose
    /// length is the value that has been returned, but nothing has been written in `out`.
    /// If the function returns value inferior or equal to `out_len` (and different from 0), then
    /// a message has been written in `out`.
    ///
    /// Messages, amongst the set that matches `to_poll`, are always returned in the order they
    /// have been received. In particular, this function does **not** search the queue of messages
    /// for a message that fits in `out_len`. It will however skip the messages in the queue that
    /// do not match any entry in `to_poll`.
    ///
    /// Messages written in `out` can be decoded into a [`Message`].
    ///
    /// When this function is being called, a "lock" is being held on the memory pointed by
    /// `to_poll` and `out`. In particular, it is invalid to modify these buffers while the
    /// function is running.
    pub(crate) fn next_message(
        to_poll: *mut u64,
        to_poll_len: u32,
        out: *mut u8,
        out_len: u32,
        block: bool,
    ) -> u32;

    /// Sends a message to the process that has registered the given interface.
    ///
    /// The memory area pointed to by `msg_bufs_ptrs` must contain a list of `msg_bufs_num` pairs
    /// of two 32-bits values encoded in little endian. In other words, the list must contain
    /// `msg_bufs_num * 2` values. Each pair is composed of a memory address and a length
    /// referring to a buffer containing a slice of the message body.
    /// The message body consists of the concatenation of all these buffers.
    ///
    /// > **Note**: This API is similar to the one of the `writev` POSIX function. The
    /// >           `msg_bufs_ptrs` parameter is similar to the `iov` parameter of `writev`, and
    /// >           the `msg_bufs_num` parameter is similar to the `iovcnt` parameter of `writev`.
    ///
    /// The message body is what will go into the [`actual_data`](Message::actual_data) field of
    /// the [`Message`] that the target will receive.
    ///
    /// Returns `0` on success, and `1` in case of error.
    ///
    /// On success, if `needs_answer` is true, will write the ID of new event into the memory
    /// pointed by `message_id_out`.
    ///
    /// If `allow_delay` is true, the kernel is allowed to block the thread in order to
    /// lazily-load a handler for that interface if necessary. If `allow_delay` is false and no
    /// interface handler is available, the function fails immediately.
    ///
    /// When this function is being called, a "lock" is being held on the memory pointed by
    /// `interface_hash`, `msg_bufs_ptrs`, `message_id_out`, and all the sub-buffers referred to
    /// within `msg_bufs_ptrs`. In particular, it is invalid to modify these buffers while the
    /// function is running.
    // TODO: document error that can happen
    pub(crate) fn emit_message(
        interface_hash: *const u8,
        msg_bufs_ptrs: *const u8,
        msg_bufs_num: u32,
        needs_answer: bool,
        allow_delay: bool,
        message_id_out: *mut u64,
    ) -> u32;

    /// Sends an answer back to the emitter of given `message_id`.
    ///
    /// When this function is being called, a "lock" is being held on the memory pointed by
    /// `message_id` and `msg`. In particular, it is invalid to modify these buffers while the
    /// function is running.
    pub(crate) fn emit_answer(message_id: *const u64, msg: *const u8, msg_len: u32);

    /// Notifies the kernel that the given message is invalid and cannot reasonably be answered.
    ///
    /// This should be used in situations where a message we receive fails to parse or is generally
    /// invalid. In other words, this should only be used in case of misbehaviour by the sender.
    ///
    /// When this function is being called, a "lock" is being held on the memory pointed by
    /// `message_id`. In particular, it is invalid to modify these buffers while the function is
    /// running.
    pub(crate) fn emit_message_error(message_id: *const u64);

    /// Cancel an expected answer.
    ///
    /// After a message that needs an answer has been emitted using `emit_message`,
    /// the `cancel_message` function can be used to signal that we're not interested in the
    /// answer.
    ///
    /// After this function has been called, the passed `message_id` is no longer valid.
    ///
    /// When this function is being called, a "lock" is being held on the memory pointed by
    /// `message_id`. In particular, it is invalid to modify this buffer while the function is
    /// running.
    pub(crate) fn cancel_message(message_id: *const u64);
}

#[derive(Debug, Clone, Encode, Decode)]
pub enum Message {
    Interface(InterfaceMessage),
    Response(ResponseMessage),
    /// Whenever a process that has emitted events on one of our interfaces stops, a
    /// `ProcessDestroyed` message is sent.
    ProcessDestroyed(ProcessDestroyedMessage),
}

#[derive(Debug, Clone, Encode, Decode, PartialEq, Eq)]
pub struct InterfaceMessage {
    /// Interface the message concerns.
    pub interface: [u8; 32],
    /// Id of the message. Can be used for answering. `None` if no answer is expected.
    pub message_id: Option<MessageId>,
    /// Id of the process that emitted the message. `None` if message was emitted by kernel.
    ///
    /// This should be used for security purposes, so that a process can't modify another process'
    /// resources.
    pub emitter_pid: Pid,
    /// Index within the list to poll where this message was.
    pub index_in_list: u32,
    pub actual_data: Vec<u8>,
}

#[derive(Debug, Clone, Encode, Decode, PartialEq, Eq)]
pub struct ProcessDestroyedMessage {
    /// Identifier of the process that got destroyed.
    pub pid: Pid,
    /// Index within the list to poll where this message was.
    pub index_in_list: u32,
}

#[derive(Debug, Clone, Encode, Decode, PartialEq, Eq)]
pub enum InterfaceOrDestroyed {
    Interface(InterfaceMessage),
    ProcessDestroyed(ProcessDestroyedMessage),
}

#[derive(Debug, Clone, Encode, Decode)]
pub struct ResponseMessage {
    /// Identifier of the message whose answer we are receiving.
    pub message_id: MessageId,

    /// Index within the list to poll where this message was.
    pub index_in_list: u32,

    /// The response, or `Err` if:
    ///
    /// - The interface handler has crashed.
    /// - The interface handler marked our message as invalid.
    ///
    pub actual_data: Result<Vec<u8>, ()>,
}
