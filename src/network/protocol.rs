use crate::error::NetworkStatsError;
use crate::frame_info::PlayerInput;
use crate::network::compression::{decode, encode};
use crate::network::messages::{
    ChecksumReport, ConnectionStatus, Input, InputAck, Message, MessageBody, MessageHeader,
    QualityReply, QualityReport,
};
use crate::time_sync::TimeSync;
use crate::{Config, DesyncDetection, Frame, NonBlockingSocket, PlayerHandle, NULL_FRAME};
use tracing::{trace, warn};

use instant::{Duration, Instant};
use std::collections::vec_deque::Drain;
use std::collections::{BTreeMap, BTreeSet, HashMap, VecDeque};
use std::convert::TryFrom;
use std::ops::Add;

use super::network_stats::NetworkStats;

const UDP_HEADER_SIZE: usize = 28; // Size of IP + UDP headers
const UDP_SHUTDOWN_TIMER: u64 = 5000;
const PENDING_OUTPUT_SIZE: usize = 128;
const RUNNING_RETRY_INTERVAL: Duration = Duration::from_millis(200);
const KEEP_ALIVE_INTERVAL: Duration = Duration::from_millis(200);
const QUALITY_REPORT_INTERVAL: Duration = Duration::from_millis(200);
/// Number of old checksums to keep in memory
pub const MAX_CHECKSUM_HISTORY_SIZE: usize = 32;

fn millis_since_epoch() -> u128 {
    #[cfg(not(target_arch = "wasm32"))]
    {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("Time went backwards")
            .as_millis()
    }
    #[cfg(target_arch = "wasm32")]
    {
        js_sys::Date::new_0().get_time() as u128
    }
}

// byte-encoded data representing the inputs of a client, possibly for multiple players at the same time
#[derive(Clone)]
struct InputBytes {
    /// The frame to which this info belongs to. -1/[`NULL_FRAME`] represents an invalid frame
    pub frame: Frame,
    /// An input buffer that will hold input data
    pub bytes: Vec<u8>,
}

impl InputBytes {
    fn zeroed<T: Config>(num_handles: usize) -> Self {
        let size = core::mem::size_of::<T::Input>() * num_handles;
        Self {
            frame: NULL_FRAME,
            bytes: vec![0; size],
        }
    }

    fn from_inputs<T: Config>(inputs: &BTreeMap<PlayerHandle, PlayerInput<T::Input>>) -> Self {
        let mut frame = NULL_FRAME;
        let mut handles = Vec::new();
        let mut bytes = Vec::new();
        // handles (& hence inputs) are guaranteed to be in handle-order due to being in a BTreeMap,
        // which makes it possible to restore the mapping from handles to input index
        for (handle, input) in inputs.iter() {
            assert!(frame == NULL_FRAME || input.frame == NULL_FRAME || frame == input.frame);
            if input.frame != NULL_FRAME {
                frame = input.frame;
            }

            handles.push(*handle);

            bincode::serialize_into(&mut bytes, &input.input).expect("input serialization failed");
        }
        Self { frame, bytes }
    }

    fn to_player_inputs<T: Config>(
        &self,
        handles: &BTreeSet<PlayerHandle>,
    ) -> HashMap<PlayerHandle, PlayerInput<T::Input>> {
        let mut player_inputs = HashMap::new();
        assert!(
            self.bytes.len() % handles.len() == 0,
            "input bytes length must be a multiple of player handle count"
        );
        let per_player_size = self.bytes.len() / handles.len();
        // since handles are in handle-order due to being in a BTreeSet, we can just iterate over
        // the handles in order and they should match the order that inputs were serialized into
        // bytes - as long as the known handles are the same in each case.
        for (handle, player_byte_slice) in
            handles.iter().zip(self.bytes.chunks_exact(per_player_size))
        {
            let input: T::Input =
                bincode::deserialize(player_byte_slice).expect("input deserialization failed");
            player_inputs.insert(*handle, PlayerInput::new(self.frame, input));
        }
        player_inputs
    }
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) enum Event<T>
where
    T: Config,
{
    /// The session has received an input from the remote client. This event will not be forwarded to the user.
    Input {
        input: PlayerInput<T::Input>,
        player: PlayerHandle,
    },
    /// The remote client has disconnected.
    Disconnected,
    /// The session has not received packets from the remote client since `disconnect_timeout` ms.
    NetworkInterrupted { disconnect_timeout: u128 },
    /// Sent only after a `NetworkInterrupted` event, if communication has resumed.
    NetworkResumed,
}

#[derive(Debug, PartialEq, Eq)]
enum ProtocolState {
    Running,
    Disconnected,
    Shutdown,
}

pub(crate) struct UdpProtocol<T>
where
    T: Config,
{
    handles: BTreeSet<PlayerHandle>,
    send_queue: VecDeque<Message>,
    event_queue: VecDeque<Event<T>>,

    // state
    state: ProtocolState,
    running_last_quality_report: Instant,
    running_last_input_recv: Instant,
    disconnect_notify_sent: bool,
    disconnect_event_sent: bool,

    // constants
    disconnect_timeout: Duration,
    disconnect_notify_start: Duration,
    shutdown_timeout: Instant,
    fps: usize,
    magic: u16,

    // the other client
    peer_addr: T::Address,
    // remote_magic: u16,
    peer_connect_status: HashMap<PlayerHandle, ConnectionStatus>,

    // input compression
    pending_output: VecDeque<InputBytes>, // input we're going to send
    last_acked_input: InputBytes,         // we delta-encode inputs we send relative to this
    max_prediction: usize,
    recv_inputs: HashMap<Frame, InputBytes>, // we delta-decode inputs received relative to these old received inputs

    // time sync
    time_sync_layer: TimeSync,
    local_frame_advantage: i32,
    remote_frame_advantage: i32,

    // network
    stats_start_time: u128,
    packets_sent: usize,
    bytes_sent: usize,
    round_trip_time: u128,
    last_send_time: Instant,
    last_recv_time: Instant,

    // debug desync
    pub(crate) pending_checksums: HashMap<Frame, u128>,
    desync_detection: DesyncDetection,
}

impl<T: Config> PartialEq for UdpProtocol<T> {
    fn eq(&self, other: &Self) -> bool {
        self.peer_addr == other.peer_addr
    }
}

impl<T: Config> UdpProtocol<T> {
    pub(crate) fn new(
        // handles == the handles that are at the address that corresponds to this endpoint
        handles: BTreeSet<PlayerHandle>,
        peer_addr: T::Address,
        // playing_players == the player handles that exist in the game as actual players
        // (not spectators)
        playing_player_handles: BTreeSet<PlayerHandle>,
        // local player handles == the player handles that we send inputs for (to this remote client)
        // e.g. for a spectator, we send input for all players
        // e.g. for p2p session remote players, we send input for only local players
        local_player_handles: BTreeSet<PlayerHandle>,
        max_prediction: usize,
        disconnect_timeout: Duration,
        disconnect_notify_start: Duration,
        fps: usize,
        desync_detection: DesyncDetection,
    ) -> Self {
        let mut magic = rand::random::<u16>();
        while magic == 0 {
            magic = rand::random::<u16>();
        }

        // peer connection status
        let peer_connect_status = playing_player_handles
            .into_iter()
            .map(|h| (h, ConnectionStatus::default()))
            .collect();

        // received input history
        let mut recv_inputs = HashMap::new();

        // zeroed input is put into the received inputs so that it can be read to be the reference
        // when we're decoding the very first received input
        recv_inputs.insert(NULL_FRAME, InputBytes::zeroed::<T>(handles.len()));

        Self {
            handles,
            send_queue: VecDeque::new(),
            event_queue: VecDeque::new(),

            // state
            state: ProtocolState::Running,
            running_last_quality_report: Instant::now(),
            running_last_input_recv: Instant::now(),
            disconnect_notify_sent: false,
            disconnect_event_sent: false,

            // constants
            disconnect_timeout,
            disconnect_notify_start,
            shutdown_timeout: Instant::now(),
            fps,
            magic,

            // the other client
            peer_addr,
            peer_connect_status,

            // input compression
            pending_output: VecDeque::with_capacity(PENDING_OUTPUT_SIZE),
            last_acked_input: InputBytes::zeroed::<T>(local_player_handles.len()),
            max_prediction,
            recv_inputs,

            // time sync
            time_sync_layer: TimeSync::new(),
            local_frame_advantage: 0,
            remote_frame_advantage: 0,

            // network
            stats_start_time: millis_since_epoch(),
            packets_sent: 0,
            bytes_sent: 0,
            round_trip_time: 0,
            last_send_time: Instant::now(),
            last_recv_time: Instant::now(),

            // debug desync
            pending_checksums: HashMap::new(),
            desync_detection,
        }
    }

    pub(crate) fn update_local_frame_advantage(&mut self, local_frame: Frame) {
        if local_frame == NULL_FRAME || self.last_recv_frame() == NULL_FRAME {
            return;
        }
        // Estimate which frame the other client is on by looking at the last frame they gave us plus some delta for the packet roundtrip time.
        let ping = i32::try_from(self.round_trip_time / 2).expect("Ping is higher than i32::MAX");
        let remote_frame = self.last_recv_frame() + ((ping * self.fps as i32) / 1000);
        // Our frame "advantage" is how many frames behind the remote client we are. (It's an advantage because they will have to predict more often)
        self.local_frame_advantage = remote_frame - local_frame;
    }

    pub(crate) fn network_stats(&self) -> Result<NetworkStats, NetworkStatsError> {
        if self.state != ProtocolState::Running {
            return Err(NetworkStatsError::Unavailable);
        }

        let now = millis_since_epoch();
        let seconds = (now - self.stats_start_time) / 1000;
        if seconds == 0 {
            return Err(NetworkStatsError::Unavailable);
        }

        let total_bytes_sent = self.bytes_sent + (self.packets_sent * UDP_HEADER_SIZE);
        let bps = total_bytes_sent / seconds as usize;
        //let upd_overhead = (self.packets_sent * UDP_HEADER_SIZE) / self.bytes_sent;

        Ok(NetworkStats {
            ping: self.round_trip_time,
            send_queue_len: self.pending_output.len(),
            kbps_sent: bps / 1024,
            local_frames_behind: self.local_frame_advantage,
            remote_frames_behind: self.remote_frame_advantage,
        })
    }

    pub(crate) fn handles(&self) -> &BTreeSet<PlayerHandle> {
        &self.handles
    }

    pub(crate) fn is_running(&self) -> bool {
        self.state == ProtocolState::Running
    }

    pub(crate) fn is_handling_message(&self, addr: &T::Address) -> bool {
        self.peer_addr == *addr
    }

    pub(crate) fn peer_connect_status(&self, handle: PlayerHandle) -> ConnectionStatus {
        tracing::info!(
            "getting peer connect status for handle {:?}; protocol peer connect status is {:?}",
            handle,
            self.peer_connect_status
        );
        self.peer_connect_status[&handle]
    }

    pub(crate) fn disconnect(&mut self) {
        if self.state == ProtocolState::Shutdown {
            return;
        }

        self.state = ProtocolState::Disconnected;
        // schedule the timeout which will lead to shutdown
        self.shutdown_timeout = Instant::now().add(Duration::from_millis(UDP_SHUTDOWN_TIMER))
    }

    pub(crate) fn average_frame_advantage(&self) -> i32 {
        self.time_sync_layer.average_frame_advantage()
    }

    pub(crate) fn peer_addr(&self) -> T::Address {
        self.peer_addr.clone()
    }

    pub(crate) fn poll(
        &mut self,
        connect_status: &HashMap<PlayerHandle, ConnectionStatus>,
    ) -> Drain<Event<T>> {
        let now = Instant::now();
        match self.state {
            ProtocolState::Running => {
                // resend pending inputs, if some time has passed without sending or receiving inputs
                if self.running_last_input_recv + RUNNING_RETRY_INTERVAL < now {
                    self.send_pending_output(connect_status);
                    self.running_last_input_recv = Instant::now();
                }

                // periodically send a quality report
                if self.running_last_quality_report + QUALITY_REPORT_INTERVAL < now {
                    self.send_quality_report();
                }

                // send keep alive packet if we didn't send a packet for some time
                if self.last_send_time + KEEP_ALIVE_INTERVAL < now {
                    self.send_keep_alive();
                }

                // trigger a NetworkInterrupted event if we didn't receive a packet for some time
                if !self.disconnect_notify_sent
                    && self.last_recv_time + self.disconnect_notify_start < now
                {
                    let duration: Duration = self.disconnect_timeout - self.disconnect_notify_start;
                    self.event_queue.push_back(Event::NetworkInterrupted {
                        disconnect_timeout: Duration::as_millis(&duration),
                    });
                    self.disconnect_notify_sent = true;
                }

                // if we pass the disconnect_timeout threshold, send an event to disconnect
                if !self.disconnect_event_sent
                    && self.last_recv_time + self.disconnect_timeout < now
                {
                    self.event_queue.push_back(Event::Disconnected);
                    self.disconnect_event_sent = true;
                }
            }
            ProtocolState::Disconnected => {
                if self.shutdown_timeout < Instant::now() {
                    self.state = ProtocolState::Shutdown;
                }
            }
            ProtocolState::Shutdown => (),
        }
        self.event_queue.drain(..)
    }

    fn pop_pending_output(&mut self, ack_frame: Frame) {
        while !self.pending_output.is_empty() {
            if let Some(input) = self.pending_output.front() {
                if input.frame <= ack_frame {
                    self.last_acked_input = self
                        .pending_output
                        .pop_front()
                        .expect("Expected input to exist");
                } else {
                    break;
                }
            }
        }
    }

    /*
     *  SENDING MESSAGES
     */

    pub(crate) fn send_all_messages(
        &mut self,
        socket: &mut Box<dyn NonBlockingSocket<T::Address>>,
    ) {
        if self.state == ProtocolState::Shutdown {
            trace!(
                "Protocol is shutting down; dropping {} messages",
                self.send_queue.len()
            );
            self.send_queue.drain(..);
            return;
        }

        if self.send_queue.is_empty() {
            // avoid log spam if there's nothing to send
            return;
        }

        trace!("Sending {} messages over socket", self.send_queue.len());
        for msg in self.send_queue.drain(..) {
            socket.send_to(&msg, &self.peer_addr);
        }
    }

    pub(crate) fn send_input(
        &mut self,
        inputs: &BTreeMap<PlayerHandle, PlayerInput<T::Input>>,
        connect_status: &HashMap<PlayerHandle, ConnectionStatus>,
    ) {
        if self.state != ProtocolState::Running {
            return;
        }

        let endpoint_data = InputBytes::from_inputs::<T>(inputs);

        // register the input and advantages in the time sync layer
        self.time_sync_layer.advance_frame(
            endpoint_data.frame,
            self.local_frame_advantage,
            self.remote_frame_advantage,
        );

        self.pending_output.push_back(endpoint_data);

        // we should never have so much pending input for a remote player (if they didn't ack, we should stop at MAX_PREDICTION_THRESHOLD)
        // this is a spectator that didn't ack our input, we just disconnect them
        if self.pending_output.len() > PENDING_OUTPUT_SIZE {
            self.event_queue.push_back(Event::Disconnected);
        }

        self.send_pending_output(connect_status);
    }

    fn send_pending_output(&mut self, connect_status: &HashMap<PlayerHandle, ConnectionStatus>) {
        if let Some(oldest_input_to_send) = self.pending_output.front() {
            assert!(
                self.last_acked_input.frame == NULL_FRAME
                    || self.last_acked_input.frame + 1 == oldest_input_to_send.frame
            );

            // encode all pending inputs to a byte buffer
            let bytes = encode(
                &self.last_acked_input.bytes,
                self.pending_output
                    .iter()
                    .map(|gi| gi.bytes.as_slice())
                    .collect(),
            );
            trace!(
                "Encoded {} bytes from {} pending output(s) into {} bytes",
                {
                    let mut sum = 0;
                    for gi in self.pending_output.iter() {
                        sum += gi.bytes.len();
                    }
                    sum
                },
                self.pending_output.len(),
                bytes.len()
            );

            self.queue_message(MessageBody::Input(Input {
                peer_connect_status: connect_status.clone(),
                disconnect_requested: self.state == ProtocolState::Disconnected,
                start_frame: oldest_input_to_send.frame,
                ack_frame: self.last_recv_frame(),
                bytes,
            }));
        }
    }

    fn send_input_ack(&mut self) {
        let body = InputAck {
            ack_frame: self.last_recv_frame(),
        };

        self.queue_message(MessageBody::InputAck(body));
    }

    fn send_keep_alive(&mut self) {
        self.queue_message(MessageBody::KeepAlive);
    }

    fn send_quality_report(&mut self) {
        self.running_last_quality_report = Instant::now();
        let body = QualityReport {
            frame_advantage: i16::try_from(
                self.local_frame_advantage
                    .clamp(i16::MIN as i32, i16::MAX as i32),
            )
            .expect("local_frame_advantage should have been clamped into the range of an i16"),
            ping: millis_since_epoch(),
        };

        self.queue_message(MessageBody::QualityReport(body));
    }

    fn queue_message(&mut self, body: MessageBody) {
        trace!("Queuing message to {:?}: {:?}", self.peer_addr, body);

        // set the header
        let header = MessageHeader { magic: self.magic };
        let msg = Message { header, body };

        self.packets_sent += 1;
        self.last_send_time = Instant::now();
        self.bytes_sent += std::mem::size_of_val(&msg);

        // add the packet to the back of the send queue
        self.send_queue.push_back(msg);
    }

    /*
     *  RECEIVING MESSAGES
     */

    pub(crate) fn handle_message(&mut self, msg: &Message) {
        trace!("Handling message from {:?}: {:?}", self.peer_addr, msg);

        // don't handle messages if shutdown
        if self.state == ProtocolState::Shutdown {
            trace!("Protocol is shutting down; ignoring message");
            return;
        }

        // update time when we last received packages
        self.last_recv_time = Instant::now();

        // if the connection has been marked as interrupted, send an event to signal we are receiving again
        if self.disconnect_notify_sent && self.state == ProtocolState::Running {
            trace!("Received message on interrupted protocol; sending NetworkResumed event");
            self.disconnect_notify_sent = false;
            self.event_queue.push_back(Event::NetworkResumed);
        }

        // handle the message
        match &msg.body {
            MessageBody::Input(body) => self.on_input(body),
            MessageBody::InputAck(body) => self.on_input_ack(*body),
            MessageBody::QualityReport(body) => self.on_quality_report(body),
            MessageBody::QualityReply(body) => self.on_quality_reply(body),
            MessageBody::ChecksumReport(body) => self.on_checksum_report(body),
            MessageBody::KeepAlive => (),
        }
    }

    fn on_input(&mut self, body: &Input) {
        // drop pending outputs until the ack frame
        self.pop_pending_output(body.ack_frame);

        // update the peer connection status
        if body.disconnect_requested {
            // if a disconnect is requested, disconnect now
            if self.state != ProtocolState::Disconnected && !self.disconnect_event_sent {
                self.event_queue.push_back(Event::Disconnected);
                self.disconnect_event_sent = true;
            }
        } else {
            // update the peer connection status
            for (handle, peer_connect_status) in self.peer_connect_status.iter_mut() {
                let Some(body_peer_connect_status) = body.peer_connect_status.get(handle) else {
                    // We have a handle that the peer does not know about; normally we would expect
                    // the peer to send us the connection status for all handles since they should
                    // have the same handles as us. So, this peer is misbehaving but we don't want
                    // to allow remote clients to cause a crash, so we just log a warning.
                    warn!("Remote peer did not include connection status for handle {handle:?}");
                    continue;
                };

                peer_connect_status.disconnected =
                    body_peer_connect_status.disconnected || peer_connect_status.disconnected;
                peer_connect_status.last_frame = std::cmp::max(
                    peer_connect_status.last_frame,
                    body_peer_connect_status.last_frame,
                );
            }
        }

        // if the encoded packet is decoded with an input we did not receive yet, we cannot recover
        assert!(
            self.last_recv_frame() == NULL_FRAME || self.last_recv_frame() + 1 >= body.start_frame
        );

        // if we did not receive any input yet, we decode with the blank input,
        // otherwise we use the input previous to the start of the encoded inputs
        let decode_frame = if self.last_recv_frame() == NULL_FRAME {
            NULL_FRAME
        } else {
            body.start_frame - 1
        };

        // if we have the necessary input saved, we decode
        if let Some(recv_inputs) = self
            .recv_inputs
            .get(&decode_frame)
            // silently drop invalid inputs (don't assert since we don't want to panic on input
            // sent by malicious clients)
            .and_then(|base| decode(&base.bytes, body.bytes.as_slice()).ok())
        {
            self.running_last_input_recv = Instant::now();

            for (i, inp) in recv_inputs.into_iter().enumerate() {
                let inp_frame = body.start_frame + i as i32;
                // skip inputs that we don't need
                if inp_frame <= self.last_recv_frame() {
                    continue;
                }

                let input_data = InputBytes {
                    frame: inp_frame,
                    bytes: inp,
                };
                // send the input to the session
                let player_inputs = input_data.to_player_inputs::<T>(&self.handles);
                self.recv_inputs.insert(input_data.frame, input_data);

                for (player, input) in player_inputs.into_iter() {
                    self.event_queue.push_back(Event::Input { input, player });
                }
            }

            // send an input ack
            self.send_input_ack();

            // delete received inputs that are too old
            let last_recv_frame = self.last_recv_frame();
            self.recv_inputs
                .retain(|&k, _| k >= last_recv_frame - 2 * self.max_prediction as i32);
        }
    }

    /// Upon receiving a `InputAck`, discard the oldest buffered input including the acked input.
    fn on_input_ack(&mut self, body: InputAck) {
        self.pop_pending_output(body.ack_frame);
    }

    /// Upon receiving a `QualityReport`, update network stats and reply with a `QualityReply`.
    fn on_quality_report(&mut self, body: &QualityReport) {
        self.remote_frame_advantage = body.frame_advantage as i32;
        let reply_body = QualityReply { pong: body.ping };
        self.queue_message(MessageBody::QualityReply(reply_body));
    }

    /// Upon receiving a `QualityReply`, update network stats.
    fn on_quality_reply(&mut self, body: &QualityReply) {
        let millis = millis_since_epoch();
        assert!(millis >= body.pong);
        self.round_trip_time = millis - body.pong;
    }

    /// Upon receiving a `ChecksumReport`, add it to the checksum history
    fn on_checksum_report(&mut self, body: &ChecksumReport) {
        let interval = if let DesyncDetection::On { interval } = self.desync_detection {
            interval
        } else {
            debug_assert!(
                false,
                "Received checksum report, but desync detection is off. Check
                that configuration is consistent between peers."
            );
            1
        };

        if self.pending_checksums.len() >= MAX_CHECKSUM_HISTORY_SIZE {
            let oldest_frame_to_keep =
                body.frame - (MAX_CHECKSUM_HISTORY_SIZE as i32 - 1) * interval as i32;
            self.pending_checksums
                .retain(|&frame, _| frame >= oldest_frame_to_keep);
        }
        self.pending_checksums.insert(body.frame, body.checksum);
    }

    /// Returns the frame of the last received input
    fn last_recv_frame(&self) -> Frame {
        match self.recv_inputs.iter().max_by_key(|&(k, _)| k) {
            Some((k, _)) => *k,
            None => NULL_FRAME,
        }
    }

    pub(crate) fn send_checksum_report(&mut self, frame_to_send: Frame, checksum: u128) {
        let body = ChecksumReport {
            frame: frame_to_send,
            checksum,
        };
        self.queue_message(MessageBody::ChecksumReport(body));
    }
}
