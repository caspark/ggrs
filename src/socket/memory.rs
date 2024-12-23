use std::{ops::Deref, sync::Arc};

use parking_lot::Mutex;

use crate::NonBlockingSocket;

#[derive(Debug, Clone)]
pub(crate) struct MemoryMsg {
    from: MemoryAddress,
    to: MemoryAddress,
    data: Vec<u8>,
}

#[derive(Debug, Clone)]
pub(crate) struct MemoryTransport {
    all_messages: Arc<Mutex<Vec<MemoryMsg>>>,
}

impl MemoryTransport {
    pub(crate) fn new() -> Self {
        Self {
            all_messages: Arc::new(Mutex::new(Vec::new())),
        }
    }
}

pub(crate) struct MemoryNetwork {
    transport: MemoryTransport,
    next_socket_address: MemoryAddress,
}
impl MemoryNetwork {
    pub fn new() -> Self {
        Self {
            transport: MemoryTransport::new(),
            next_socket_address: 0,
        }
    }

    pub fn add_socket(&mut self) -> MemorySocket {
        let address = self.next_socket_address;
        self.next_socket_address += 1;

        MemorySocket {
            address,
            transport: self.transport.clone(),
        }
    }
}

type MemoryAddress = usize;

pub(crate) struct MemorySocket {
    address: MemoryAddress,
    transport: MemoryTransport,
}

impl NonBlockingSocket<MemoryAddress> for MemorySocket {
    fn send_to(&mut self, buf: &[u8], addr: &MemoryAddress) {
        self.transport.all_messages.lock().push(MemoryMsg {
            from: self.address,
            to: *addr,
            data: buf.to_vec(),
        });
    }

    fn receive_all_messages(&mut self) -> Vec<(MemoryAddress, Vec<u8>)> {
        let mut received = Vec::new();
        self.transport.all_messages.lock().retain(|msg| {
            if msg.to == self.address {
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
    use proptest::prelude::*;
    use std::collections::HashMap;

    proptest! {
        #[test]
        fn test_memory_socket_messaging(
            // Generate between 2-10 messages for each socket pair
            messages in prop::collection::vec(any::<[u8; 2]>(), 2..10),
            // Generate between 2-5 sockets to test with
            num_sockets in 2..5usize,
        ) {
            // Set up the network and sockets
            let mut network = MemoryNetwork::new();
            let mut sockets: Vec<MemorySocket> = (0..num_sockets)
                .map(|_| network.add_socket())
                .collect();

            // Keep track of what messages each socket should receive
            let mut expected_messages: HashMap<MemoryAddress, Vec<(MemoryAddress, Vec<u8>)>> =
                HashMap::new();

            // Send all messages between random pairs of sockets
            for message_data in messages {
                // Pick random sender and receiver
                let sender_idx = message_data[0] as usize % num_sockets;
                let receiver_idx = (message_data[0] as usize + 1) % num_sockets; // Ensure different from sender

                let sender_addr = sockets[sender_idx].address;
                let receiver_addr = sockets[receiver_idx].address;

                // Send the message
                sockets[sender_idx].send_to(&message_data, &receiver_addr);

                // Record the expected message
                expected_messages
                    .entry(receiver_addr)
                    .or_default()
                    .push((sender_addr, message_data.to_vec()));
            }

            // Verify each socket receives exactly what it should
            for socket in sockets.iter_mut() {
                let received = socket.receive_all_messages();
                let expected = expected_messages.get(&socket.address).cloned().unwrap_or_default();

                // Sort both vectors to make comparison stable
                let mut received_sorted = received;
                let mut expected_sorted = expected;
                received_sorted.sort_by_key(|k| (k.0, k.1.clone()));
                expected_sorted.sort_by_key(|k| (k.0, k.1.clone()));

                prop_assert_eq!(
                    received_sorted,
                    expected_sorted,
                    "Socket {} received incorrect messages",
                    socket.address
                );
            }

            // Verify all messages have been cleared from transport
            prop_assert_eq!(
                sockets[0].transport.all_messages.lock().len(),
                0,
                "Messages remained in transport after receiving"
            );
        }
    }
}
