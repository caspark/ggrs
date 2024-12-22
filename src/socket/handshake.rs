use core::panic;
use std::{collections::HashMap, fmt::Debug};

use serde::{Deserialize, Serialize};
use tracing::trace;

use crate::{Address, NonBlockingSocket};

/// The handshaking process consists of sending a sequence of "sync numbers" to the remote endpoint;
/// once the socket has received each of the numbers back again as a response, the handshake is
/// considered complete.
type SyncNumber = u32;

/// Once the handshake is complete, the socket will only accept messages from an address if the
/// header id matches the one used in the final handshake.
///
/// In [GGPO](https://github.com/pond3r/ggpo), this header id is referred to as the `magic`.
type HeaderId = u16;

/// Some data sent or received by this socket.
#[derive(Debug, Serialize, Deserialize)]
enum HandshakeMessage {
    SyncRequest(SyncNumber),
    SyncResponse {
        ack_sync_number: SyncNumber,
        remote_header_id: HeaderId,
    },
    /// Data sent or received by the user of the socket.
    Data {
        header_id: HeaderId,
        buf: Vec<u8>,
    },
}

/// The state machine that controls the handshaking process.
#[derive(Debug)]
struct HandshakeState {
    /// Initial numbers to send to remote endpoint
    unsent: Vec<SyncNumber>,
    /// Numbers we've sent to the remote endpoint but haven't received back as a response yet
    awaiting: Vec<SyncNumber>,
    /// Count of numbers we've received back from the endpoint
    recv_count: usize,
    remote_header_id: Option<HeaderId>,
}

impl HandshakeState {
    pub fn new(unsent: Vec<u32>) -> Self {
        Self {
            unsent,
            awaiting: Vec::new(),
            recv_count: 0,
            remote_header_id: None,
        }
    }

    /// Returns the next sync number to send, or None if handshaking is complete.
    fn next_sync_number(&mut self) -> Option<SyncNumber> {
        if let Some(new_sync_number) = self.unsent.pop() {
            self.awaiting.push(new_sync_number);
            Some(new_sync_number)
        } else if let Some(&old_sync_number) = self.awaiting.first() {
            // move the number we just pulled to the back of the list, so that next time we send a
            // sync packet, we'll send the next-oldest one.
            self.awaiting.rotate_left(1);
            Some(old_sync_number)
        } else {
            trace!("No sync numbers left to send so handshaking should now be complete");
            None
        }
    }

    /// Returns true if the given sync number matches one of the expected sync numbers.
    fn acknowledge_sync_response(&mut self, sync_number: SyncNumber) -> bool {
        if let Some(index) = self.awaiting.iter().position(|n| *n == sync_number) {
            self.awaiting.remove(index);
            self.recv_count += 1;
            true
        } else {
            false
        }
    }

    fn completed(&self) -> bool {
        self.remote_header_id.is_some()
    }

    pub fn status(&self) -> HandshakeStatus {
        HandshakeStatus {
            received: self.recv_count,
            remaining: self.unsent.len() + self.awaiting.len(),
        }
    }
}

pub struct HandshakeStatus {
    received: usize,
    remaining: usize,
}
impl HandshakeStatus {
    /// Return true if the handshake is completed.
    pub fn completed(&self) -> bool {
        self.remaining == 0
    }

    /// Return a number from 0 (not started) to 1 (completed) indicating how much of the handshake
    /// has been completed, suitable for displaying in progress bars or similar.
    pub fn completed_frac(&self) -> f32 {
        self.received as f32 / (self.received as f32 + self.remaining as f32)
    }
}
impl std::fmt::Display for HandshakeStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{:.0}%", self.completed_frac() * 100.0)
    }
}
impl std::fmt::Debug for HandshakeStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("HandshakeStatus")
            .field("received", &self.received)
            .field("remaining", &self.remaining)
            .finish()
    }
}

/// A socket that wraps another socket and drops all messages sent to or received from any given
/// address until it has completed a handshake process with that address.
///
/// You must use [`HandshakingSocket::register()`] to register a remote address with the handshaking
/// socket before any data is sent to it.
///
/// # Handshake Purpose
///
/// When wrapping a [UdpNonBlockingSocket], this handshaking process serves a few functions:
/// * It allows confirming that the remote endpoint is able to receive our packets and we can
///   receive packets from it. This allows you to avoid creating GGRS sessions with endpoints that
///   are unreachable (which would result in those remote players being disconnected after a
///   timeout).
/// * It provides very basic authentication mechanism that prevents an attacker from spoofing the
///   from address of a legitimate remote endpoint and sending us inputs (however if they can
///   intercept traffic, then they will still be able to spoof input from the remote address).
///
/// If you are using a network transport other than [`UdpNonBlockingSocket`] which provides its own
/// handshaking and authentication, this wrapper is unnecessary: using it will just make your GGRS
/// session take longer to actually start advancing game states.
///
/// # Waiting for Handshaking
///
/// Once you have registered a remote address with the handshaking socket, you may choose whether or
/// not to wait for the handshake to complete before starting the GGRS session.
///
/// To wait for the handshake to complete before starting the rollback session, you can use
/// [`HandshakingSocket::poll()`] (in each client) to poll the handshake status of all registered
/// addresses until [`HandshakingSocket::handshake_status()`] reports that all handshakes have
/// completed.
///
/// If you do not wait for handshaking to complete, note that if handshaking fails with a remote
/// player, then remote player will be disconnected from the session. GGRS does not factor in the
/// handshaking process when disconnecting unresponsive remote players, so you should test in your
/// worst supported network conditions to ensure that your session disconnect timeout is high enough
/// that clients are not disconnected by the timeout before the session is complete. Typically (if
/// the GGRS session is polled at 60HZ) handshaking takes under 2 seconds.
///
/// Each "half" of a client-client pair can independently decide whether to wait (or not) for
/// handshaking to complete, though both clients must still use a [`HandshakingSocket`] if either
/// client uses one.
///
/// # GGPO Equivalence
///
/// In [GGPO](https://github.com/pond3r/ggpo), this handshaking process is called "Synchronization".
/// However in GGPO synchronization must always complete before the rollback session can advance any
/// frames.
///
/// # Implementation
///
/// The actual handshake process is implementation defined and subject to change, but currently it
/// consists of sending a sequence of random numbers to the remote address and waiting for those
/// numbers to be echoed back to us. Once the handshake is complete, the socket will record the
/// identity of the remote endpoint based on a random identifier (which is received along with the
/// received final random number), and further messages that do not include that known header id
/// will be dropped.
pub struct HandshakingSocket<A: Address, S: NonBlockingSocket<A>> {
    wrapped: S,
    header_id: HeaderId,
    handshakes: HashMap<A, HandshakeState>,
}

impl<A: Address, S: NonBlockingSocket<A>> HandshakingSocket<A, S> {
    /// Wraps some other [`NonBlockingSocket`] to add handshaking capabilities to it, generating
    /// a random header id for this socket.
    #[cfg(feature = "rand")]
    pub fn wrap(socket: S) -> Self {
        Self::wrap_with_id(socket, rand::random::<HeaderId>())
    }

    /// Wraps some other [`NonBlockingSocket`] to add handshaking capabilities to it.
    pub fn wrap_with_id(socket: S, header_id: HeaderId) -> Self {
        Self {
            wrapped: socket,
            header_id,
            handshakes: HashMap::new(),
        }
    }

    /// Prepares a handshake with a remote client, using randomly generated data for the handshake.
    #[cfg(feature = "rand")]
    pub fn register(&mut self, addr: A) {
        let sync_numbers = (0..5).map(|_| rand::random::<SyncNumber>()).collect();
        self.register_with_data(addr, sync_numbers);
    }

    /// Prepares a handshake with a remote client, using the given sync numbers.
    ///
    /// Registering a handshake
    ///
    /// Note that more sync numbers will make the handshake take longer.
    ///
    /// This API is unstable and will change as needed by changes to the implementation of
    /// handshaking, but specifying the sync numbers explicitly may be useful to increase
    /// determinism in your application or in test code.
    pub fn register_with_data(&mut self, addr: A, sync_numbers: Vec<SyncNumber>) {
        // If the user does not want to use handshaking, then they should not wrap their socket in a
        // handshaking socket - so we always require at least one sync number to be specified.
        assert!(
            !sync_numbers.is_empty(),
            "Provided sync numbers must not be empty"
        );

        self.handshakes
            .insert(addr, HandshakeState::new(sync_numbers));
    }

    /// Advance socket handshaking states for all registered addresses.
    ///
    /// It is only necessary to call this if the socket is not being asked to send and receive
    /// messages to/from addresses by a GGRS session, as normally polling a GGRS session will cause
    /// the handshaking process to progress automatically.
    ///
    /// For example, it may be worthwhile calling this in a loop before the socket is passed to a
    /// GGRS session if you want to be certain that all handshakes have completed before the GGRS
    /// session starts. Use [`status()`][HandshakingSocket::status] to check the status of
    /// handshakes.
    ///
    /// NB: Calling this will drop any non-handshake messages received from the socket, even for
    /// addresses whose handshaking state has been completed. (This is not a problem for GGRS as it
    /// expects its transport to be unreliable.)
    pub fn poll(&mut self) {
        for addr in self.handshakes.keys().cloned().collect::<Vec<_>>() {
            NonBlockingSocket::send_to(self, &[], &addr);
        }
        NonBlockingSocket::receive_all_messages(self);
    }

    /// Returns the status of handshakes with all addresses.
    pub fn handshake_status(&self) -> HashMap<A, HandshakeStatus> {
        self.handshakes
            .iter()
            .map(|(addr, handshake)| (addr.clone(), handshake.status()))
            .collect()
    }
}

fn send_message<A: Address>(
    socket: &mut impl NonBlockingSocket<A>,
    addr: &A,
    msg: HandshakeMessage,
) {
    let buf = bincode::serialize(&msg).expect("Serialization to handshake packet should succeed");
    socket.send_to(buf.as_slice(), addr);
}

impl<A: Address, S: NonBlockingSocket<A>> NonBlockingSocket<A> for HandshakingSocket<A, S> {
    fn send_to(&mut self, buf: &[u8], addr: &A) {
        let Some(handshake) = self.handshakes.get_mut(addr) else {
            panic!(
                "Address {:?} is unknown to HandshakingSocket so cannot send data to it; \
                register it first.",
                addr
            );
        };

        if let Some(remote_header_id) = handshake.remote_header_id {
            // handshake is done - we can send the data straight away and then we're done
            send_message(
                &mut self.wrapped,
                addr,
                HandshakeMessage::Data {
                    header_id: remote_header_id,
                    buf: buf.to_vec(),
                },
            );
            return;
        }

        // Handshake is not done yet so we need to keep synchronizing instead. (We'll just drop the
        // message we were given for now; ggrs expects its transports to be unreliable, so at worst
        // this will simply move a remote endpoint further along the disconnection timer.)

        let Some(next_sync_number) = handshake.next_sync_number() else {
            panic!("Handshaking should only ever complete when we just received a sync reply");
        };
        send_message(
            &mut self.wrapped,
            addr,
            HandshakeMessage::SyncRequest(next_sync_number),
        );
    }

    fn receive_all_messages(&mut self) -> Vec<(A, Vec<u8>)> {
        self.wrapped
            .receive_all_messages()
            .into_iter()
            .filter_map(|(addr, bytes)| {
                // we must avoid panicking for any remote input
                let Ok(msg): Result<HandshakeMessage, _> = bincode::deserialize(&bytes) else {
                    trace!("Dropping invalid handshake packet received from {addr:?} : {bytes:?}",);
                    return None;
                };
                let Some(handshake_state) = self.handshakes.get_mut(&addr) else {
                    trace!("Dropping handshake packet received from unknown address: {addr:?}",);
                    return None;
                };

                match msg {
                    HandshakeMessage::SyncRequest(sync_number) => {
                        // send a response with the same number we just received, since that's what
                        // the remote endpoint is expecting
                        trace!(
                            "Sending sync response with sync number {sync_number:?} to {addr:?}",
                        );
                        send_message(
                            &mut self.wrapped,
                            &addr,
                            HandshakeMessage::SyncResponse {
                                ack_sync_number: sync_number,
                                remote_header_id: self.header_id,
                            },
                        );
                        return None;
                    }
                    HandshakeMessage::SyncResponse {
                        ack_sync_number: sync_number,
                        remote_header_id,
                    } => {
                        if !handshake_state.acknowledge_sync_response(sync_number) {
                            trace!(
                                "Dropping sync response with sync number \
                                {sync_number:?} we didn't send",
                            );
                            return None;
                        }

                        if let Some(next_sync_number) = handshake_state.next_sync_number() {
                            trace!(
                                "Sending sync request with sync number {next_sync_number:?} \
                                to {addr:?}",
                            );
                            send_message(
                                &mut self.wrapped,
                                &addr,
                                HandshakeMessage::SyncRequest(next_sync_number),
                            );
                            return None;
                        } else {
                            trace!(
                                "Handshake with {addr:?} is complete; \
                            setting remote header id to {remote_header_id:?}"
                            );
                            handshake_state.remote_header_id = Some(remote_header_id);
                            return None;
                        }
                    }
                    HandshakeMessage::Data { header_id, buf } => {
                        if header_id != self.header_id {
                            trace!(
                                "Dropping handshake packet received from {addr:?} with \
                                invalid header id {header_id} (expected {})",
                                self.header_id,
                            );
                            return None;
                        }
                        if !handshake_state.completed() {
                            trace!(
                                "Dropping data received from {addr:?} before handshake was \
                                completed",
                            );
                            return None;
                        }
                        Some((addr, buf))
                    }
                }
            })
            .collect()
    }
}
