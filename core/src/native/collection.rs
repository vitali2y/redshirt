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

use crate::native::traits::{NativeProgramEvent, NativeProgramMessageIdWrite, NativeProgramRef};

use alloc::{boxed::Box, vec::Vec};
use core::{mem, task::Context, task::Poll};
use futures::prelude::*;
use hashbrown::HashSet;
use redshirt_interface_interface::ffi::InterfaceMessage;
use redshirt_syscalls_interface::{Decode as _, EncodedMessage, InterfaceHash, MessageId, Pid};
use spin::Mutex;

/// Collection of objects that implement the [`NativeProgram`] trait.
pub struct NativeProgramsCollection<'ext> {
    /// Collection of processes and their `Pid`.
    processes: Vec<(Pid, Box<dyn AdapterAbstract + Send + 'ext>)>,
}

/// Event generated by a [`NativeProgram`].
pub enum NativeProgramsCollectionEvent<'col> {
    /// Request to emit a message.
    Emit {
        /// Interface to emit the message on.
        interface: InterfaceHash,
        /// Pid of the program that emits the message. Same as a value that was passed to
        /// [`push`](NativeProgramsCollection::push).
        emitter_pid: Pid,
        /// Emitted message.
        message: EncodedMessage,
        /// If `Some`, must be used to write the [`MessageId`].
        message_id_write: Option<NativeProgramsCollectionMessageIdWrite<'col>>,
    },
    /// Request to cancel a previously-emitted message.
    CancelMessage {
        /// Message to cancel.
        message_id: MessageId,
    },
    /// Request to answer a message received with
    /// [`interface_message`](NativeProgramsCollection::interface_message).
    Answer {
        /// Message to answer.
        message_id: MessageId,
        /// The produced answer, or an `Err` if the message is invalid.
        answer: Result<EncodedMessage, ()>,
    },
}

/// Allows writing back a [`MessageId`] when a message is emitted.
#[must_use]
pub struct NativeProgramsCollectionMessageIdWrite<'col> {
    write: Box<dyn AbstractMessageIdWrite + 'col>,
}

/// Wraps around a [`NativeProgram`].
struct Adapter<T> {
    inner: T,
    registered_interfaces: Mutex<HashSet<InterfaceHash>>,
    expected_responses: Mutex<HashSet<MessageId>>,
}

/// Abstracts over [`Adapter`] so that we can box it.
trait AdapterAbstract {
    fn poll_next_event<'col>(
        &'col self,
        cx: &mut Context,
    ) -> Poll<NativeProgramEvent<Box<dyn AbstractMessageIdWrite + 'col>>>;
    fn deliver_interface_message(
        &self,
        interface: InterfaceHash,
        message_id: Option<MessageId>,
        emitter_pid: Pid,
        message: EncodedMessage,
    ) -> Result<(), EncodedMessage>;
    fn deliver_response(
        &self,
        message_id: MessageId,
        response: Result<EncodedMessage, ()>,
    ) -> Result<(), Result<EncodedMessage, ()>>;
    fn process_destroyed(&self, pid: Pid);
}

trait AbstractMessageIdWrite {
    fn acknowledge(&mut self, id: MessageId);
}

struct MessageIdWriteAdapter<'col, T> {
    inner: Option<T>,
    expected_responses: &'col Mutex<HashSet<MessageId>>,
}

impl<'ext> NativeProgramsCollection<'ext> {
    /// Builds an empty collection.
    ///
    /// Calling [`next_event`](NativeProgramsCollection::next_event) will never yield anything.
    pub fn new() -> Self {
        NativeProgramsCollection {
            processes: Vec::new(),
        }
    }

    /// Adds a program to the collection.
    ///
    /// # Panic
    ///
    /// Panics if the `pid` already exists in this collection.
    ///
    pub fn push<T>(&mut self, pid: Pid, program: T)
    where
        T: Send + 'ext,
        for<'r> &'r T: NativeProgramRef<'r>,
    {
        let adapter = Box::new(Adapter {
            inner: program,
            registered_interfaces: Mutex::new(HashSet::new()),
            expected_responses: Mutex::new(HashSet::new()),
        });

        assert!(!self
            .processes
            .iter()
            .any(|(existing_pid, _)| *existing_pid == pid));
        self.processes.push((pid, adapter));

        // We assume that `push` is only ever called at initialization.
        self.processes.shrink_to_fit();
    }

    /// Returns a `Future` that yields the next event generated by one of the programs.
    pub fn next_event<'collec>(
        &'collec self,
    ) -> impl Future<Output = NativeProgramsCollectionEvent<'collec>> + 'collec {
        future::poll_fn(move |cx| {
            for (pid, process) in self.processes.iter() {
                match process.poll_next_event(cx) {
                    Poll::Pending => {}
                    Poll::Ready(NativeProgramEvent::Emit {
                        interface,
                        message_id_write,
                        message,
                    }) => {
                        return Poll::Ready(NativeProgramsCollectionEvent::Emit {
                            emitter_pid: *pid,
                            interface,
                            message,
                            message_id_write: message_id_write
                                .map(|w| NativeProgramsCollectionMessageIdWrite { write: w }),
                        })
                    }
                    Poll::Ready(NativeProgramEvent::CancelMessage { message_id }) => {
                        return Poll::Ready(NativeProgramsCollectionEvent::CancelMessage {
                            message_id,
                        })
                    }
                    Poll::Ready(NativeProgramEvent::Answer { message_id, answer }) => {
                        return Poll::Ready(NativeProgramsCollectionEvent::Answer {
                            message_id,
                            answer,
                        })
                    }
                }
            }

            Poll::Pending
        })
    }

    /// Notify the [`NativeProgram`] that a message has arrived on one of the interface that it
    /// has registered.
    pub fn interface_message(
        &self,
        interface: InterfaceHash,
        message_id: Option<MessageId>,
        emitter_pid: Pid,
        mut message: EncodedMessage,
    ) {
        for (_, process) in &self.processes {
            let msg = mem::replace(&mut message, EncodedMessage(Vec::new()));
            match process.deliver_interface_message(interface.clone(), message_id, emitter_pid, msg)
            {
                Ok(_) => return,
                Err(msg) => message = msg,
            }
        }

        panic!() // TODO: what to do here?
    }

    /// Notify the [`NativeProgram`]s that the program with the given [`Pid`] has terminated.
    pub fn process_destroyed(&mut self, pid: Pid) {
        for (_, process) in &self.processes {
            process.process_destroyed(pid);
        }
    }

    /// Notify the appropriate [`NativeProgram`] of a response to a message that it has previously
    /// emitted.
    pub fn message_response(
        &self,
        message_id: MessageId,
        mut response: Result<EncodedMessage, ()>,
    ) {
        for (_, process) in &self.processes {
            let msg = mem::replace(&mut response, Ok(EncodedMessage(Vec::new())));
            match process.deliver_response(message_id, msg) {
                Ok(_) => return,
                Err(msg) => response = msg,
            }
        }

        panic!() // TODO: what to do here?
    }
}

impl<T> AdapterAbstract for Adapter<T>
where
    for<'r> &'r T: NativeProgramRef<'r>,
{
    fn poll_next_event<'col>(
        &'col self,
        cx: &mut Context,
    ) -> Poll<NativeProgramEvent<Box<dyn AbstractMessageIdWrite + 'col>>> {
        let future = (&self.inner).next_event();
        futures::pin_mut!(future);
        match future.poll(cx) {
            Poll::Ready(NativeProgramEvent::Emit {
                interface,
                message_id_write,
                message,
            }) => {
                if interface == redshirt_interface_interface::ffi::INTERFACE {
                    // TODO: check whether registration succeeds, but hard if `message_id_write` is `None
                    if let Ok(msg) = InterfaceMessage::decode(message.clone()) {
                        let InterfaceMessage::Register(to_reg) = msg;
                        let mut registered_interfaces = self.registered_interfaces.lock();
                        registered_interfaces.insert(to_reg);
                    }
                }

                let message_id_write = message_id_write.map(|inner| {
                    Box::new(MessageIdWriteAdapter {
                        inner: Some(inner),
                        expected_responses: &self.expected_responses,
                    }) as Box<_>
                });

                Poll::Ready(NativeProgramEvent::Emit {
                    interface,
                    message,
                    message_id_write,
                })
            }
            Poll::Ready(NativeProgramEvent::CancelMessage { message_id }) => {
                Poll::Ready(NativeProgramEvent::CancelMessage { message_id })
            }
            Poll::Ready(NativeProgramEvent::Answer { message_id, answer }) => {
                Poll::Ready(NativeProgramEvent::Answer { message_id, answer })
            }
            Poll::Pending => Poll::Pending,
        }
    }

    fn deliver_interface_message(
        &self,
        interface: InterfaceHash,
        message_id: Option<MessageId>,
        emitter_pid: Pid,
        message: EncodedMessage,
    ) -> Result<(), EncodedMessage> {
        let registered_interfaces = self.registered_interfaces.lock();
        if registered_interfaces.contains(&interface) {
            self.inner
                .interface_message(interface, message_id, emitter_pid, message);
            Ok(())
        } else {
            Err(message)
        }
    }

    fn deliver_response(
        &self,
        message_id: MessageId,
        response: Result<EncodedMessage, ()>,
    ) -> Result<(), Result<EncodedMessage, ()>> {
        let mut expected_responses = self.expected_responses.lock();
        if expected_responses.remove(&message_id) {
            self.inner.message_response(message_id, response);
            Ok(())
        } else {
            Err(response)
        }
    }

    fn process_destroyed(&self, pid: Pid) {
        self.inner.process_destroyed(pid);
    }
}

impl<'col, T> AbstractMessageIdWrite for MessageIdWriteAdapter<'col, T>
where
    T: NativeProgramMessageIdWrite,
{
    fn acknowledge(&mut self, id: MessageId) {
        match self.inner.take() {
            Some(inner) => inner.acknowledge(id),
            None => unreachable!(),
        };
        let _was_inserted = self.expected_responses.lock().insert(id);
        debug_assert!(_was_inserted);
    }
}

impl<'col> NativeProgramMessageIdWrite for NativeProgramsCollectionMessageIdWrite<'col> {
    fn acknowledge(mut self, message_id: MessageId) {
        self.write.acknowledge(message_id);
    }
}

// TODO: impl<'col> NativeProgram<'col> for NativeProgramsCollection<'col>

#[cfg(test)]
mod tests {
    use super::NativeProgramsCollection;

    #[test]
    fn is_send() {
        fn req_send<T: Send>() {}
        req_send::<NativeProgramsCollection>();
    }
}
