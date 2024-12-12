use std::{
    io::ErrorKind,
    net::{IpAddr, Ipv4Addr, SocketAddr, UdpSocket},
};

use crate::{logging::warn, network::messages::Message, NonBlockingSocket};

const RECV_BUFFER_SIZE: usize = 4096;
/// A packet larger than this may be fragmented, so ideally we wouldn't send packets larger than
/// this.
/// Source: https://stackoverflow.com/a/35697810/775982
const IDEAL_MAX_UDP_PACKET_SIZE: usize = 508;

/// A simple non-blocking UDP socket tu use with GGRS Sessions. Listens to 0.0.0.0 on a given port.
#[derive(Debug)]
pub struct UdpNonBlockingSocket {
    socket: UdpSocket,
    buffer: [u8; RECV_BUFFER_SIZE],
}

impl UdpNonBlockingSocket {
    /// Binds an UDP Socket to 0.0.0.0:port and set it to non-blocking mode.
    pub fn bind_to_port(port: u16) -> Result<Self, std::io::Error> {
        let addr = SocketAddr::new(IpAddr::V4(Ipv4Addr::UNSPECIFIED), port);
        let socket = UdpSocket::bind(addr)?;
        socket.set_nonblocking(true)?;
        Ok(Self {
            socket,
            buffer: [0; RECV_BUFFER_SIZE],
        })
    }
}

impl NonBlockingSocket<SocketAddr> for UdpNonBlockingSocket {
    fn send_to(&mut self, msg: &Message, addr: &SocketAddr) {
        let buf = bincode::serialize(&msg).unwrap();

        // warn for large packets that may be fragmented
        if buf.len() > IDEAL_MAX_UDP_PACKET_SIZE {
            warn!(
                "Sending UDP packet of size {} bytes, which is \
                larger than ideal ({IDEAL_MAX_UDP_PACKET_SIZE})",
                buf.len()
            );
        }

        self.socket.send_to(&buf, addr).unwrap();
    }

    fn receive_all_messages(&mut self) -> Vec<(SocketAddr, Message)> {
        let mut received_messages = Vec::new();
        loop {
            match self.socket.recv_from(&mut self.buffer) {
                Ok((number_of_bytes, src_addr)) => {
                    assert!(number_of_bytes <= RECV_BUFFER_SIZE);
                    if let Ok(msg) = bincode::deserialize(&self.buffer[0..number_of_bytes]) {
                        received_messages.push((src_addr, msg));
                    }
                }
                // there are no more messages
                Err(ref err) if err.kind() == ErrorKind::WouldBlock => return received_messages,
                // datagram socket sometimes get this error as a result of calling the send_to method
                Err(ref err) if err.kind() == ErrorKind::ConnectionReset => continue,
                // all other errors cause a panic
                Err(err) => panic!("{:?}: {} on {:?}", err.kind(), err, &self.socket),
            }
        }
    }
}
