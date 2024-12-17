use std::error::Error;
use std::fmt;
use std::fmt::Display;

use crate::Frame;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum NetworkStatsError {
    /// Network statistics are unavailable if no time has elapsed or if the peer has been disconnected.
    Unavailable,
    /// Network statistics were requested for an invalid player handle.
    BadPlayerHandle,
}
impl Display for NetworkStatsError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            NetworkStatsError::Unavailable => {
                write!(f, "Network statistics are unavailable for this player.")
            }
            NetworkStatsError::BadPlayerHandle => {
                write!(
                    f,
                    "Network statistics were requested for an invalid player handle."
                )
            }
        }
    }
}

/// This enum contains all error messages this library can return. Most API functions will generally return a [`Result<(), GgrsError>`].
///
/// [`Result<(), GgrsError>`]: std::result::Result
#[derive(Debug, Clone, PartialEq, Hash)]
pub enum GgrsError {
    /// When the prediction threshold has been reached, we cannot accept more inputs from the local player.
    PredictionThreshold,
    /// You made an invalid request, usually by using wrong parameters for function calls.
    InvalidRequest {
        /// Further specifies why the request was invalid.
        info: String,
    },
    /// In a [`SyncTestSession`], this error is returned if checksums of resimulated frames do not match up with the original checksum.
    ///
    /// [`SyncTestSession`]: crate::SyncTestSession
    MismatchedChecksum {
        /// The frame at which the mismatch occurred.
        current_frame: Frame,
        /// The frames with mismatched checksums (one or more)
        mismatched_frames: Vec<Frame>,
    },
    /// The Session is not synchronized yet. Please start the session and wait a few ms to let the clients synchronize.
    NotSynchronized,
    /// The spectator got so far behind the host that catching up is impossible.
    SpectatorTooFarBehind,
}

impl Display for GgrsError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            GgrsError::PredictionThreshold => {
                write!(
                    f,
                    "Prediction threshold is reached, cannot proceed without catching up."
                )
            }
            GgrsError::InvalidRequest { info } => {
                write!(f, "Invalid Request: {}", info)
            }
            GgrsError::NotSynchronized => {
                write!(
                    f,
                    "The session is not yet synchronized with all remote sessions."
                )
            }
            GgrsError::MismatchedChecksum {
                current_frame,
                mismatched_frames,
            } => {
                write!(
                    f,
                    "Detected checksum mismatch during rollback on frame {}, mismatched frames: {:?}",
                    current_frame, mismatched_frames
                )
            }
            GgrsError::SpectatorTooFarBehind => {
                write!(
                    f,
                    "The spectator got so far behind the host that catching up is impossible."
                )
            }
        }
    }
}

impl Error for GgrsError {}
