use crate::NonBlockingSocket;
use parking_lot::Mutex;
use std::sync::Arc;

type MemoryAddress = usize;

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

#[derive(Debug, Clone)]
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

    pub fn num_sockets(&self) -> usize {
        self.next_socket_address
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

#[derive(Debug)]
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

    #[test]
    fn test_basic_memory_socket_communication() {
        let mut network = MemoryNetwork::new();
        let mut socket1 = network.add_socket();
        let mut socket2 = network.add_socket();

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
        assert_eq!(socket1.transport.all_messages.lock().len(), 0);

        let message3 = vec![9, 10, 11, 12];
        socket1.send_to(&message3, &socket2.address);

        // Verify messages are only delivered to intended recipients
        assert!(socket1.receive_all_messages().is_empty());
        let received_by_socket2 = socket2.receive_all_messages();
        assert_eq!(received_by_socket2.len(), 1);
        assert_eq!(received_by_socket2[0].1, message3);
    }
}
