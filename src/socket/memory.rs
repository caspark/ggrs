use crate::NonBlockingSocket;
use parking_lot::Mutex;
use rand::prelude::*;
use std::sync::Arc;

type MemoryAddress = usize;
type VirtualTime = u64;

#[derive(Debug, Clone)]
pub struct SocketConfig {
    // Base latency in virtual milliseconds
    latency: VirtualTime,
    // Percentage chances (0-100)
    loss_percent: u8,
    corruption_percent: u8,
    reordering_percent: u8,
    duplication_percent: u8,
}

impl Default for SocketConfig {
    fn default() -> Self {
        Self {
            latency: 0,
            loss_percent: 0,
            corruption_percent: 0,
            reordering_percent: 0,
            duplication_percent: 0,
        }
    }
}

#[derive(Debug, Clone)]
struct NetworkState {
    current_time: VirtualTime,
    rng: StdRng,
    // Map from address to its config
    socket_configs: Vec<SocketConfig>,
}

impl NetworkState {
    fn new(seed: u64) -> Self {
        Self {
            current_time: 0,
            rng: StdRng::seed_from_u64(seed),
            socket_configs: Vec::new(),
        }
    }

    fn advance_time(&mut self, duration: VirtualTime) {
        self.current_time += duration;
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
    // Messages waiting to be delivered, sorted by delivery time
    pending_messages: Vec<MemoryMsg>,
    // Network state including time and RNG
    network_state: NetworkState,
}

impl MemoryTransport {
    pub(crate) fn new(seed: u64) -> Self {
        Self {
            pending_messages: Vec::new(),
            network_state: NetworkState::new(seed),
        }
    }

    pub(crate) fn add_socket_config(&mut self, config: SocketConfig) -> MemoryAddress {
        let addr = self.network_state.socket_configs.len();
        self.network_state.socket_configs.push(config);
        addr
    }

    fn should_drop(&mut self, from: MemoryAddress) -> bool {
        let config = &self.network_state.socket_configs[from];
        self.network_state.rng.gen_range(0..100) < config.loss_percent
    }

    fn should_corrupt(&mut self, from: MemoryAddress) -> bool {
        let config = &self.network_state.socket_configs[from];
        self.network_state.rng.gen_range(0..100) < config.corruption_percent
    }

    fn should_reorder(&mut self, from: MemoryAddress) -> bool {
        let config = &self.network_state.socket_configs[from];
        self.network_state.rng.gen_range(0..100) < config.reordering_percent
    }

    fn should_duplicate(&mut self, from: MemoryAddress) -> bool {
        let config = &self.network_state.socket_configs[from];
        self.network_state.rng.gen_range(0..100) < config.duplication_percent
    }

    fn get_latency(&mut self, from: MemoryAddress) -> VirtualTime {
        self.network_state.socket_configs[from].latency
    }

    fn corrupt_data(&mut self, mut data: Vec<u8>) -> Vec<u8> {
        if data.is_empty() {
            return data;
        }
        // Corrupt a random byte
        let pos = self.network_state.rng.gen_range(0..data.len());
        data[pos] = self.network_state.rng.gen();
        data
    }
}

#[derive(Debug, Clone)]
pub(crate) struct MemoryNetwork {
    transport: Arc<Mutex<MemoryTransport>>,
    next_socket_address: MemoryAddress,
}

impl MemoryNetwork {
    pub fn new(seed: u64) -> Self {
        Self {
            transport: Arc::new(Mutex::new(MemoryTransport::new(seed))),
            next_socket_address: 0,
        }
    }

    pub fn add_socket(&mut self, config: SocketConfig) -> MemorySocket {
        let mut transport = self.transport.lock();
        let address = transport.add_socket_config(config);
        self.next_socket_address = address + 1;

        MemorySocket {
            address,
            transport: self.transport.clone(),
        }
    }

    pub fn num_sockets(&self) -> usize {
        self.next_socket_address
    }
}

#[derive(Debug)]
pub(crate) struct MemorySocket {
    address: MemoryAddress,
    transport: Arc<Mutex<MemoryTransport>>,
}

impl NonBlockingSocket<MemoryAddress> for MemorySocket {
    fn send_to(&mut self, buf: &[u8], addr: &MemoryAddress) {
        let mut transport = self.transport.lock();

        // Check for packet loss
        if transport.should_drop(self.address) {
            return;
        }

        // Get base latency and prepare data
        let mut data = buf.to_vec();
        let delivery_time =
            transport.network_state.current_time + transport.get_latency(self.address);

        // Apply corruption if needed
        if transport.should_corrupt(self.address) {
            data = transport.corrupt_data(data);
        }

        // Create the base message
        let msg = MemoryMsg {
            from: self.address,
            to: *addr,
            data,
            delivery_time,
        };

        // Handle duplication
        if transport.should_duplicate(self.address) {
            // Duplicate arrives a bit later
            let duplicate = MemoryMsg {
                delivery_time: delivery_time + 1,
                ..msg.clone()
            };
            transport.pending_messages.push(duplicate);
        }

        // Handle reordering by randomly adjusting delivery time
        let msg = if transport.should_reorder(self.address) {
            MemoryMsg {
                // Reordered messages arrive earlier
                delivery_time: delivery_time.saturating_sub(2),
                ..msg
            }
        } else {
            msg
        };

        transport.pending_messages.push(msg);
    }

    fn receive_all_messages(&mut self) -> Vec<(MemoryAddress, Vec<u8>)> {
        let mut transport = self.transport.lock();
        let current_time = transport.network_state.current_time;

        let mut received = Vec::new();
        transport.pending_messages.retain(|msg| {
            if msg.to == self.address && msg.delivery_time <= current_time {
                received.push((msg.from, msg.data.clone()));
                false
            } else {
                true
            }
        });

        // Advance time by 1 unit after each receive operation
        transport.network_state.advance_time(1);
        received
    }
}

#[cfg(test)]
mod memory_tests {
    use super::*;

    #[test]
    fn test_basic_memory_socket_communication() {
        let mut network = MemoryNetwork::new(12345);
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

    use proptest::collection::{hash_map, vec};
    use proptest::prelude::*;
    use std::collections::HashMap;

    #[derive(Debug, Clone)]
    struct MessageRecord {
        from: MemoryAddress,
        to: MemoryAddress,
        data: Vec<u8>,
        sequence: usize,
    }

    proptest! {
        #[test]
        fn test_memory_socket_comprehensive(
            // Generate initial number of sockets (2-16)
            initial_sockets in 2..=16usize,
            // Generate sequence of operations:
            // (add_socket, from_socket, to_socket, message_data)
            operations in vec(
                (
                    // 20% chance to add a new socket
                    prop::bool::weighted(0.2),
                    // Source socket index
                    any::<usize>(),
                    // Destination socket index
                    any::<usize>(),
                    // Message data (max 2KB)
                    vec(any::<u8>(), 1..=2048),
                ),
                1..=100 // Up to 100 operations
            )
        ) {
            let mut network = MemoryNetwork::new(12345);
            let mut sockets = Vec::new();
            let mut socket_addresses = Vec::new();
            let mut expected_messages: Vec<MessageRecord> = Vec::new();
            let mut sequence = 0usize;

            // Create initial sockets with default config (no failures)
            for _ in 0..initial_sockets {
                let socket = network.add_socket(SocketConfig::default());
                socket_addresses.push(socket.address);
                sockets.push(socket);
            }

            // Track actual number of sockets for bounds checking
            prop_assert_eq!(network.num_sockets(), initial_sockets);

            // Process each operation
            for (add_socket, from_idx, to_idx, data) in operations {
                if add_socket {
                    let socket = network.add_socket(SocketConfig::default());
                    socket_addresses.push(socket.address);
                    sockets.push(socket);
                    prop_assert_eq!(network.num_sockets(), sockets.len());
                }

                // Ensure socket indices are valid
                let from_idx = from_idx % sockets.len();
                let to_idx = to_idx % sockets.len();

                // Send message
                sockets[from_idx].send_to(&data, &socket_addresses[to_idx]);

                // Record expected message
                expected_messages.push(MessageRecord {
                    from: socket_addresses[from_idx],
                    to: socket_addresses[to_idx],
                    data,
                    sequence,
                });
                sequence += 1;
            }

            // Verify all messages are received correctly and in order
            let mut received_by_socket: HashMap<MemoryAddress, Vec<MessageRecord>> = HashMap::new();

            // Collect all received messages
            for i in 0..sockets.len() {
                let messages = sockets[i].receive_all_messages();
                let socket_addr = socket_addresses[i];
                for (from_addr, data) in messages {
                    received_by_socket
                        .entry(socket_addr)
                        .or_default()
                        .push(MessageRecord {
                            from: from_addr,
                            to: socket_addr,
                            data,
                            sequence: 0, // Will be set when matching with expected messages
                        });
                }
            }

            // Group expected messages by destination socket
            let mut expected_by_socket: HashMap<MemoryAddress, Vec<&MessageRecord>> = HashMap::new();
            for expected in expected_messages.iter() {
                expected_by_socket
                    .entry(expected.to)
                    .or_default()
                    .push(expected);
            }

            // Verify all messages were received in order for each socket
            for (socket_addr, received_messages) in received_by_socket.iter() {
                let expected_messages = expected_by_socket
                    .get(socket_addr)
                    .expect("Should have expected messages for this socket");

                prop_assert_eq!(
                    received_messages.len(),
                    expected_messages.len(),
                    "Socket {} received wrong number of messages", socket_addr
                );

                // Match received messages with expected ones and verify order
                let mut last_sequence = None;
                for received in received_messages {
                    let matching_expected = expected_messages
                        .iter()
                        .find(|expected| {
                            expected.from == received.from &&
                            expected.to == received.to &&
                            expected.data == received.data
                        })
                        .expect("Should find matching expected message");

                    if let Some(last_seq) = last_sequence {
                        prop_assert!(
                            matching_expected.sequence > last_seq,
                            "Messages received out of order at socket {}. Message with sequence {} came after {}",
                            socket_addr,
                            matching_expected.sequence,
                            last_seq
                        );
                    }
                    last_sequence = Some(matching_expected.sequence);
                }
            }

            // Verify no unexpected messages were received
            let total_received: usize = received_by_socket
                .values()
                .map(|msgs| msgs.len())
                .sum();

            prop_assert_eq!(
                total_received,
                expected_messages.len(),
                "Number of received messages doesn't match expected"
            );

            // Verify all messages have been consumed
            for i in 0..sockets.len() {
                prop_assert!(
                    sockets[i].receive_all_messages().is_empty(),
                    "Socket should have no remaining messages"
                );
            }
        }
    }
}
