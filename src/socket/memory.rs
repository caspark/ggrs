use crate::NonBlockingSocket;
use parking_lot::{MappedMutexGuard, Mutex, MutexGuard};
use std::sync::Arc;

type MemoryAddress = usize;
type VirtualTime = u64;

#[derive(Debug, Clone)]
pub enum SocketNetworkStateChange {
    /// Set the latency of all packets sent
    SetLatency(VirtualTime),
    /// Packets sent will be duplicated
    SetDuplicating(bool),
    /// Packets sent will be dropped
    SetDropping(bool),
    /// Packets received will be out of order (applied at receiving time; last packet in queue will
    /// be received first, so must have a latency greater than 0 for this to do anything)
    SetOutOfOrderReceive(bool),
}

#[derive(Debug, Clone)]
pub struct SocketNetworkChangeEvent {
    time: VirtualTime,
    change: SocketNetworkStateChange,
}

#[derive(Debug, Clone)]
pub struct SocketConfig {
    // Next packet sent will be delayed by this many milliseconds
    send_latency: VirtualTime,
    /// Drop on send
    drop_on_send: bool,
    /// Whether packets sent will be duplicated
    duplicate_on_send: bool,
    /// Whether packets received will be out of order
    out_of_order_receiving: bool,
    // Sequence of network events still left to apply
    remaining_events: Vec<SocketNetworkChangeEvent>,
}

impl Default for SocketConfig {
    fn default() -> Self {
        Self {
            send_latency: 0,
            drop_on_send: false,
            duplicate_on_send: false,
            out_of_order_receiving: false,
            remaining_events: Vec::new(),
        }
    }
}

impl SocketConfig {
    pub fn set_events(&mut self, events: Vec<SocketNetworkChangeEvent>) {
        self.remaining_events = events;
        self.remaining_events.sort_by_key(|event| event.time);
    }
}

#[derive(Debug, Clone)]
pub(crate) struct MemoryMsg {
    from: MemoryAddress,
    to: MemoryAddress,
    data: Vec<u8>,
    delivery_time: VirtualTime,
}

#[derive(Debug, Clone)]
pub(crate) struct MemoryTransport {
    current_time: VirtualTime,
    /// Messages waiting to be delivered, sorted by delivery time
    pending_messages: Vec<MemoryMsg>,
    socket_configs: Vec<SocketConfig>,
}

impl MemoryTransport {
    pub(crate) fn new() -> Self {
        Self {
            current_time: 0,
            pending_messages: Vec::new(),
            socket_configs: Vec::new(),
        }
    }

    pub(crate) fn add_socket(&mut self, config: SocketConfig) -> MemoryAddress {
        let addr = self.socket_configs.len();
        self.socket_configs.push(config);
        addr
    }

    pub fn advance_time(&mut self, duration: VirtualTime) {
        self.current_time += duration;

        for config in self.socket_configs.iter_mut() {
            config.remaining_events.retain(|event| {
                if event.time >= self.current_time {
                    true
                } else {
                    match event.change {
                        SocketNetworkStateChange::SetLatency(latency) => {
                            config.send_latency = latency;
                        }
                        SocketNetworkStateChange::SetDuplicating(duplicating) => {
                            config.duplicate_on_send = duplicating;
                        }
                        SocketNetworkStateChange::SetDropping(dropping) => {
                            config.drop_on_send = dropping;
                        }
                        SocketNetworkStateChange::SetOutOfOrderReceive(out_of_order) => {
                            config.out_of_order_receiving = out_of_order;
                        }
                    }
                    false
                }
            });
        }
    }

    pub fn config(&self, addr: MemoryAddress) -> &SocketConfig {
        &self.socket_configs[addr]
    }

    pub fn config_mut(&mut self, addr: MemoryAddress) -> &mut SocketConfig {
        &mut self.socket_configs[addr]
    }
}

#[derive(Debug, Clone)]
pub(crate) struct MemoryNetwork {
    transport: Arc<Mutex<MemoryTransport>>,
}

impl MemoryNetwork {
    pub fn new() -> Self {
        Self {
            transport: Arc::new(Mutex::new(MemoryTransport::new())),
        }
    }

    pub fn add_socket(&mut self, config: SocketConfig) -> MemorySocket {
        let mut transport = self.transport.lock();
        let address = transport.add_socket(config);

        MemorySocket {
            address,
            transport: self.transport.clone(),
        }
    }

    pub fn num_sockets(&self) -> usize {
        self.transport.lock().socket_configs.len()
    }

    pub fn advance_time(&mut self, duration: VirtualTime) {
        self.transport.lock().advance_time(duration);
    }

    pub fn config(&mut self, addr: MemoryAddress) -> MappedMutexGuard<SocketConfig> {
        MutexGuard::map(self.transport.lock(), |t| t.config_mut(addr))
    }
}

#[derive(Debug)]
pub(crate) struct MemorySocket {
    address: MemoryAddress,
    transport: Arc<Mutex<MemoryTransport>>,
}

impl MemorySocket {
    pub fn address(&self) -> MemoryAddress {
        self.address
    }

    pub fn config_mut(&self) -> MappedMutexGuard<SocketConfig> {
        MutexGuard::map(self.transport.lock(), |t| t.config_mut(self.address))
    }
}

impl NonBlockingSocket<MemoryAddress> for MemorySocket {
    fn send_to(&mut self, buf: &[u8], addr: &MemoryAddress) {
        let mut transport = self.transport.lock();
        let sender_config = transport.config(self.address);

        if sender_config.drop_on_send {
            return;
        }

        let delivery_time = transport.current_time + sender_config.send_latency;
        let msg = MemoryMsg {
            from: self.address,
            to: *addr,
            data: buf.to_vec(),
            delivery_time,
        };

        if sender_config.duplicate_on_send {
            // Duplicate arrives at same time
            let duplicate = MemoryMsg {
                delivery_time,
                ..msg.clone()
            };
            transport.pending_messages.push(duplicate);
        }

        // (reordering is handled at receive time)

        transport.pending_messages.push(msg);
    }

    fn receive_all_messages(&mut self) -> Vec<(MemoryAddress, Vec<u8>)> {
        let mut transport = self.transport.lock();
        let mut received = Vec::new();
        let current_time = transport.current_time;

        if transport.config(self.address).out_of_order_receiving {
            // find the first message (starting from the end of the list of pending messages)
            // destined for this socket and return it as the first message (ignoring the normal
            // "delivery time" ordering)
            let index = transport
                .pending_messages
                .iter()
                .rposition(|msg| msg.to == self.address)
                .unwrap();
            let msg = transport.pending_messages.remove(index);
            received.push((msg.from, msg.data));
        }

        // now handle the normal "delivery time" based ordering, where we only return messages
        // that are destined for this socket and have a delivery time that has passed
        transport.pending_messages.retain(|msg| {
            if msg.to == self.address && msg.delivery_time <= current_time {
                received.push((msg.from, msg.data.clone()));
                false
            } else {
                true
            }
        });

        received
    }
}

#[cfg(test)]
mod memory_tests {
    use super::*;
    use proptest::collection::vec;
    use proptest::prelude::*;

    #[derive(Debug, Clone)]
    struct TestMessage {
        seq_num: u64,
        data: Vec<u8>,
        should_drop: bool,
        can_duplicate: bool,
        expected_delivery_time: VirtualTime,
        to: MemoryAddress,
    }

    prop_compose! {
        fn arb_network_event(max_time: VirtualTime)(
            time in 0..=max_time,
            change in prop_oneof![
                any::<VirtualTime>().prop_map(SocketNetworkStateChange::SetLatency),
                Just(true).prop_map(SocketNetworkStateChange::SetDropping),
                Just(false).prop_map(SocketNetworkStateChange::SetDropping),
                Just(true).prop_map(SocketNetworkStateChange::SetDuplicating),
                Just(false).prop_map(SocketNetworkStateChange::SetDuplicating)
            ]
        ) -> SocketNetworkChangeEvent {
            SocketNetworkChangeEvent { time, change }
        }
    }

    proptest! {
        //FIXME this test fails if latency is set to 1001 - probably need to have a configured max latency for arb_network_event!
        //TODO create an arb for a network with N sockets, with configurable latency, duplication, dropping, and out-of-order receiving
        #[test]
        fn test_network_state_changes(// // Generate 2-4 sockets
                num_sockets in 2..=4usize,
                // Generate up to 10 network events with max time of 1000
                events in vec(arb_network_event(1000), 0..10),
                // Generate up to 20 messages
                messages in vec(
                    (
                        // Source socket index
                        any::<MemoryAddress>(),
                        // Destination socket index
                        any::<MemoryAddress>(),
                        // Message data (1-100 bytes)
                        vec(any::<u8>(), 1..=100),
                        // Send time (0-1000)
                        0..=1000u64
                    ),
                    0..20
                )
        ) {
            let mut network = MemoryNetwork::new();
            let mut sockets = Vec::new();
            let mut socket_addresses = Vec::new();

            // Create sockets
            for _ in 0..num_sockets {
                let socket = network.add_socket(SocketConfig::default());
                socket_addresses.push(socket.address());
                sockets.push(socket);
            }

            // Set up network events
            for socket in sockets.iter_mut() {
                socket.config_mut().set_events(events.clone());
            }

            // Track expected messages
            let mut expected_messages: Vec<TestMessage> = Vec::new();
            let mut next_seq = 0u64;

            // Process messages in time order
            let mut time_events: Vec<(VirtualTime, Vec<u8>, usize, usize)> = messages
                .into_iter()
                .map(|(from, to, data, time)| (time, data, from % num_sockets, to % num_sockets))
                .collect();
            time_events.sort_by_key(|&(t, _, _, _)| t);

            let mut current_time = 0;
            for (send_time, data, from_idx, to_idx) in time_events {
                eprintln!("Sending message at time {}", send_time);

                // Advance time if needed
                if send_time > current_time {
                    network.advance_time(send_time - current_time);
                    current_time = send_time;
                }

                let test_msg = {
                    // Get current network state for sending socket
                    let sender_config = network.config(socket_addresses[from_idx]);

                    // Create test message with expected behavior
                    TestMessage {
                        seq_num: next_seq,
                        data: data.clone(),
                        should_drop: sender_config.drop_on_send,
                        can_duplicate: sender_config.duplicate_on_send,
                        expected_delivery_time: current_time + sender_config.send_latency,
                        to: socket_addresses[to_idx],
                    }
                };

                // Send the message
                sockets[from_idx].send_to(&data, &socket_addresses[to_idx]);

                if !test_msg.should_drop {
                    expected_messages.push(test_msg.clone());
                    if test_msg.can_duplicate {
                        expected_messages.push(test_msg);
                    }
                }

                next_seq += 1;
            }

            // Advance to final time to ensure all messages are delivered
            network.advance_time(1000);

            // Verify received messages match expectations
            for (socket_idx, socket) in sockets.iter_mut().enumerate() {
                let received = socket.receive_all_messages();

                // Get expected messages for this socket
                let mut expected: Vec<_> = expected_messages
                    .iter()
                    .filter(|msg| socket_addresses[socket_idx] == msg.to)
                    .collect();
                expected.sort_by_key(|msg| msg.expected_delivery_time);

                // Verify received messages match expected
                prop_assert_eq!(
                    received.len(),
                    expected.len(),
                    "Socket {} received {} messages but expected {}",
                    socket_idx,
                    received.len(),
                    expected.len()
                );

                for (recv, exp) in received.iter().zip(expected.iter()) {
                    prop_assert_eq!(
                        &recv.1,
                        &exp.data,
                        "Socket {} received unexpected message data",
                        socket_idx
                    );
                }
            }
        }
    }

    #[test]
    fn test_basic_memory_socket_communication() {
        let mut network = MemoryNetwork::new();
        let mut socket1 = network.add_socket(SocketConfig::default());
        let mut socket2 = network.add_socket(SocketConfig::default());

        socket1.send_to(&vec![1, 2, 3, 4], &socket2.address);
        socket2.send_to(&vec![5, 6, 7, 8], &socket1.address);

        let received_by_socket2 = socket2.receive_all_messages();
        assert_eq!(received_by_socket2.len(), 1);
        assert_eq!(received_by_socket2[0].0, socket1.address);
        assert_eq!(received_by_socket2[0].1, vec![1, 2, 3, 4]);

        let received_by_socket1 = socket1.receive_all_messages();
        assert_eq!(received_by_socket1.len(), 1);
        assert_eq!(received_by_socket1[0].0, socket2.address);
        assert_eq!(received_by_socket1[0].1, vec![5, 6, 7, 8]);

        // Verify cleanup behavior - messages should be removed after being received
        assert_eq!(socket1.transport.lock().pending_messages.len(), 0);

        let message3 = vec![9, 10, 11, 12];
        socket1.send_to(&message3, &socket2.address);

        // Verify messages are only delivered to intended recipients
        assert!(socket1.receive_all_messages().is_empty());
        let received_by_socket2 = socket2.receive_all_messages();
        assert_eq!(received_by_socket2.len(), 1);
        assert_eq!(received_by_socket2[0].1, message3);
    }

    proptest! {
        #[test]
        fn test_memory_socket_reliable(
            // Generate initial number of sockets (2-16)
            initial_sockets in 2..=16usize,
            // Generate sequence of send operations:
            sends in vec(
                (
                    // Source socket index
                    any::<MemoryAddress>(),
                    // Destination socket index
                    any::<MemoryAddress>(),
                    // Message data (max 2KB)
                    vec(any::<u8>(), 1..=2048),
                ),
                1..=100 // This many operations
            ),
        ) {
            let mut network = MemoryNetwork::new();
            let mut sockets = Vec::new();
            let mut socket_addresses = Vec::new();

            for _ in 0..initial_sockets {
                let socket = network.add_socket(SocketConfig::default());
                socket_addresses.push(socket.address());
                sockets.push(socket);
            }

            // Track actual number of sockets for bounds checking
            prop_assert_eq!(network.num_sockets(), initial_sockets);

            // Process each operation
            for (from_idx, to_idx, data) in sends {
                // Ensure socket indices are valid
                let from_idx = from_idx % sockets.len();
                let to_idx = to_idx % sockets.len();

                // Send message
                sockets[from_idx].send_to(&data, &socket_addresses[to_idx]);

                // Verify message is received
                let received = sockets[to_idx].receive_all_messages();
                prop_assert_eq!(received.len(), 1);
                prop_assert_eq!(received[0].0, socket_addresses[from_idx]);
                prop_assert_eq!(&received[0].1, &data);
            }
        }
    }
}
