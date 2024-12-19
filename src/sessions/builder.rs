use std::collections::{BTreeSet, HashMap};

use instant::Duration;

use crate::{
    network::protocol::UdpProtocol, sessions::p2p_session::PlayerRegistry, Config, DesyncDetection,
    GgrsError, NonBlockingSocket, P2PSession, PlayerHandle, PlayerType, SpectatorSession,
    SyncTestSession,
};

use super::p2p_spectator_session::SPECTATOR_BUFFER_SIZE;

const DEFAULT_SAVE_MODE: bool = false;
const DEFAULT_DETECTION_MODE: DesyncDetection = DesyncDetection::Off;
const DEFAULT_INPUT_DELAY: usize = 0;
const DEFAULT_DISCONNECT_TIMEOUT: Duration = Duration::from_millis(2000);
const DEFAULT_DISCONNECT_NOTIFY_START: Duration = Duration::from_millis(500);
const DEFAULT_FPS: usize = 60;
const DEFAULT_MAX_PREDICTION_FRAMES: usize = 8;
const DEFAULT_CHECK_DISTANCE: usize = 2;
// If the spectator is more than this amount of frames behind, it will advance the game two steps at a time to catch up
const DEFAULT_MAX_FRAMES_BEHIND: usize = 10;
// The amount of frames the spectator advances in a single step if too far behind
const DEFAULT_CATCHUP_SPEED: usize = 1;
// The amount of events a spectator can buffer; should never be an issue if the user polls the events at every step
pub(crate) const MAX_EVENT_QUEUE_SIZE: usize = 100;

/// The [`SessionBuilder`] builds all GGRS Sessions. After setting all appropriate values, use `SessionBuilder::start_yxz_session(...)`
/// to consume the builder and create a Session of desired type.
#[derive(Debug)]
pub struct SessionBuilder<T>
where
    T: Config,
{
    max_prediction: usize,
    /// FPS defines the expected update frequency of this session.
    fps: usize,
    sparse_saving: bool,
    desync_detection: DesyncDetection,
    /// The time until a remote player gets disconnected.
    disconnect_timeout: Duration,
    /// The time until the client will get a notification that a remote player is about to be disconnected.
    disconnect_notify_start: Duration,
    player_reg: PlayerRegistry<T>,
    input_delay: usize,
    check_dist: usize,
    max_frames_behind: usize,
    catchup_speed: usize,
}

impl<T: Config> Default for SessionBuilder<T> {
    fn default() -> Self {
        Self::new()
    }
}

impl<T: Config> SessionBuilder<T> {
    /// Construct a new builder with all values set to their defaults.
    pub fn new() -> Self {
        Self {
            player_reg: PlayerRegistry::new(),
            max_prediction: DEFAULT_MAX_PREDICTION_FRAMES,
            fps: DEFAULT_FPS,
            sparse_saving: DEFAULT_SAVE_MODE,
            desync_detection: DEFAULT_DETECTION_MODE,
            disconnect_timeout: DEFAULT_DISCONNECT_TIMEOUT,
            disconnect_notify_start: DEFAULT_DISCONNECT_NOTIFY_START,
            input_delay: DEFAULT_INPUT_DELAY,
            check_dist: DEFAULT_CHECK_DISTANCE,
            max_frames_behind: DEFAULT_MAX_FRAMES_BEHIND,
            catchup_speed: DEFAULT_CATCHUP_SPEED,
        }
    }

    /// Must be called for each player in the session (e.g. in a 3 player session, must be called 3 times) before starting the session.
    /// Player handles for players should be between 0 and `num_players`, spectator handles should be higher than `num_players`.
    /// Later, you will need the player handle to add input, change parameters or disconnect the player or spectator.
    ///
    /// # Errors
    /// - Returns [`InvalidRequest`] if a player with that handle has been added before
    /// - Returns [`InvalidRequest`] if the handle is invalid for the given [`PlayerType`]
    ///
    /// [`InvalidRequest`]: GgrsError::InvalidRequest
    /// [`num_players`]: Self#structfield.num_players
    pub fn add_player(
        mut self,
        player_type: PlayerType<T::Address>,
        player_handle: PlayerHandle,
    ) -> Result<Self, GgrsError> {
        // check if the player handle is already in use
        if self.player_reg.handles.contains_key(&player_handle) {
            return Err(GgrsError::InvalidRequest {
                info: "Player handle already in use.".to_owned(),
            });
        }
        self.player_reg.handles.insert(player_handle, player_type);
        Ok(self)
    }

    /// Change the maximum prediction window. Default is 8.
    ///
    /// ## Lockstep mode
    ///
    /// As a special case, if you set this to 0, GGRS will run in lockstep mode:
    /// * ggrs will only request that you advance the gamestate if the current frame has inputs
    ///   confirmed from all other clients.
    /// * ggrs will never request you to save or roll back the gamestate.
    ///
    /// Lockstep mode can significantly reduce the (GGRS) framerate of your game, but may be
    /// appropriate for games where a GGRS frame does not correspond to a rendered frame, such as a
    /// game where GGRS frames are only advanced once a second; with input delay set to zero, the
    /// framerate impact is approximately equivalent to taking the highest latency client and adding
    /// its latency to the current time to tick a frame.
    pub fn with_max_prediction_window(mut self, window: usize) -> Self {
        self.max_prediction = window;
        self
    }

    /// Change the amount of frames GGRS will delay the inputs for local players.
    pub fn with_input_delay(mut self, delay: usize) -> Self {
        self.input_delay = delay;
        self
    }

    /// Sets the sparse saving mode. With sparse saving turned on, only the minimum confirmed frame
    /// (for which all inputs from all players are confirmed correct) will be saved. This leads to
    /// much less save requests at the cost of potentially longer rollbacks and thus more advance
    /// frame requests. Recommended, if saving your gamestate takes much more time than advancing
    /// the game state.
    pub fn with_sparse_saving_mode(mut self, sparse_saving: bool) -> Self {
        self.sparse_saving = sparse_saving;
        self
    }

    /// Sets the desync detection mode. With desync detection, the session will compare checksums for all peers to detect discrepancies / desyncs between peers
    /// If a desync is found the session will send a DesyncDetected event.
    pub fn with_desync_detection_mode(mut self, desync_detection: DesyncDetection) -> Self {
        self.desync_detection = desync_detection;
        self
    }

    /// Sets the disconnect timeout. The session will automatically disconnect from a remote peer if it has not received a packet in the timeout window.
    pub fn with_disconnect_timeout(mut self, timeout: Duration) -> Self {
        self.disconnect_timeout = timeout;
        self
    }

    /// Sets the time before the first notification will be sent in case of a prolonged period of no received packages.
    pub fn with_disconnect_notify_delay(mut self, notify_delay: Duration) -> Self {
        self.disconnect_notify_start = notify_delay;
        self
    }

    /// Sets the FPS this session is used with. This influences estimations for frame synchronization between sessions.
    /// # Errors
    /// - Returns [`InvalidRequest`] if the fps is 0
    ///
    /// [`InvalidRequest`]: GgrsError::InvalidRequest
    pub fn with_fps(mut self, fps: usize) -> Result<Self, GgrsError> {
        if fps == 0 {
            return Err(GgrsError::InvalidRequest {
                info: "FPS should be higher than 0.".to_owned(),
            });
        }
        self.fps = fps;
        Ok(self)
    }

    /// Change the check distance. Default is 2.
    pub fn with_check_distance(mut self, check_distance: usize) -> Self {
        self.check_dist = check_distance;
        self
    }

    /// Sets the maximum frames behind. If the spectator is more than this amount of frames behind the received inputs,
    /// it will catch up with `catchup_speed` amount of frames per step.
    ///
    pub fn with_max_frames_behind(mut self, max_frames_behind: usize) -> Result<Self, GgrsError> {
        if max_frames_behind < 1 {
            return Err(GgrsError::InvalidRequest {
                info: "Max frames behind cannot be smaller than 1.".to_owned(),
            });
        }

        if max_frames_behind >= SPECTATOR_BUFFER_SIZE {
            return Err(GgrsError::InvalidRequest {
                info: "Max frames behind cannot be larger or equal than the Spectator buffer size (60)"
                    .to_owned(),
            });
        }
        self.max_frames_behind = max_frames_behind;
        Ok(self)
    }

    /// Sets the catchup speed. Per default, this is set to 1, so the spectator never catches up.
    /// If you want the spectator to catch up to the host if `max_frames_behind` is surpassed, set this to a value higher than 1.
    pub fn with_catchup_speed(mut self, catchup_speed: usize) -> Result<Self, GgrsError> {
        if catchup_speed < 1 {
            return Err(GgrsError::InvalidRequest {
                info: "Catchup speed cannot be smaller than 1.".to_owned(),
            });
        }

        if catchup_speed >= self.max_frames_behind {
            return Err(GgrsError::InvalidRequest {
                info: "Catchup speed cannot be larger or equal than the allowed maximum frames behind host"
                    .to_owned(),
            });
        }
        self.catchup_speed = catchup_speed;
        Ok(self)
    }

    /// Consumes the builder to construct a [`P2PSession`] and starts synchronization of endpoints.
    /// # Errors
    /// - Returns [`InvalidRequest`] if insufficient players have been registered.
    ///
    /// [`InvalidRequest`]: GgrsError::InvalidRequest
    pub fn start_p2p_session(
        mut self,
        socket: impl NonBlockingSocket<T::Address> + 'static,
    ) -> Result<P2PSession<T>, GgrsError> {
        // group player handles by address
        let mut addr_to_handles = HashMap::<PlayerType<T::Address>, BTreeSet<PlayerHandle>>::new();
        for (handle, player_type) in self.player_reg.handles.iter() {
            match player_type {
                PlayerType::Remote(_) | PlayerType::Spectator(_) => {
                    addr_to_handles
                        .entry(player_type.clone())
                        .or_default()
                        .insert(*handle);
                }
                PlayerType::Local => (),
            }
        }

        // for each unique address, create an endpoint
        let playing_players: BTreeSet<PlayerHandle> = self
            .player_reg
            .playing_player_handles()
            .into_iter()
            .collect();
        for (player_type, address_handles) in addr_to_handles.into_iter() {
            match player_type {
                PlayerType::Remote(peer_addr) => {
                    self.player_reg.remotes.insert(
                        peer_addr.clone(),
                        self.create_endpoint(
                            address_handles,
                            peer_addr.clone(),
                            playing_players.clone(),
                            self.player_reg.local_player_handles().into_iter().collect(),
                        ),
                    );
                }
                PlayerType::Spectator(peer_addr) => {
                    self.player_reg.spectators.insert(
                        peer_addr.clone(),
                        self.create_endpoint(
                            address_handles,
                            peer_addr.clone(),
                            playing_players.clone(),
                            // the host of the spectator pretends all players are its own local
                            // players so that it sends inputs for all players to the spectator
                            playing_players.clone(),
                        ),
                    );
                }
                PlayerType::Local => (),
            }
        }

        Ok(P2PSession::<T>::new(
            self.max_prediction,
            Box::new(socket),
            self.player_reg,
            self.sparse_saving,
            self.desync_detection,
            self.input_delay,
        ))
    }

    /// Consumes the builder to create a new [`SpectatorSession`].
    /// A [`SpectatorSession`] provides all functionality to connect to a remote host in a peer-to-peer fashion.
    /// The host will broadcast all confirmed inputs to this session.
    /// This session can be used to spectate a session without contributing to the game input.
    pub fn start_spectator_session(
        self,
        host_addr: T::Address,
        socket: impl NonBlockingSocket<T::Address> + 'static,
    ) -> SpectatorSession<T> {
        let playing_player_handles: BTreeSet<PlayerHandle> = self
            .player_reg
            .playing_player_handles()
            .into_iter()
            .collect();
        // create host endpoint
        let host = UdpProtocol::new(
            // as far as the spectator is concerned, all players are local to the host, so it
            // expects the host to send inputs for all players on this endpoint
            playing_player_handles.clone(),
            host_addr,
            playing_player_handles.clone(),
            [PlayerHandle(0)].into(), // should not matter since the spectator is never sending
            self.max_prediction,
            self.disconnect_timeout,
            self.disconnect_notify_start,
            self.fps,
            DesyncDetection::Off,
        );
        SpectatorSession::new(
            playing_player_handles,
            Box::new(socket),
            host,
            self.max_frames_behind,
            self.catchup_speed,
        )
    }

    /// Consumes the builder to construct a new [`SyncTestSession`]. During a [`SyncTestSession`], GGRS will simulate a rollback every frame
    /// and resimulate the last n states, where n is the given `check_distance`.
    /// The resimulated checksums will be compared with the original checksums and report if there was a mismatch.
    /// Due to the decentralized nature of saving and loading gamestates, checksum comparisons can only be made if `check_distance` is 2 or higher.
    /// This is a great way to test if your system runs deterministically.
    /// After creating the session, add a local player, set input delay for them and then start the session.
    pub fn start_synctest_session(self) -> Result<SyncTestSession<T>, GgrsError> {
        if self.check_dist >= self.max_prediction {
            return Err(GgrsError::InvalidRequest {
                info: "Check distance too big.".to_owned(),
            });
        }
        Ok(SyncTestSession::new(
            self.player_reg
                .local_player_handles()
                .iter()
                .copied()
                .collect(),
            self.max_prediction,
            self.check_dist,
            self.input_delay,
        ))
    }

    fn create_endpoint(
        &self,
        address_players: BTreeSet<PlayerHandle>,
        peer_addr: T::Address,
        playing_player_handles: BTreeSet<PlayerHandle>,
        local_player_handles: BTreeSet<PlayerHandle>,
    ) -> UdpProtocol<T> {
        UdpProtocol::new(
            address_players,
            peer_addr,
            playing_player_handles,
            local_player_handles,
            self.max_prediction,
            self.disconnect_timeout,
            self.disconnect_notify_start,
            self.fps,
            self.desync_detection,
        )
    }
}
