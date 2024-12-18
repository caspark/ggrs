use serde::{de::DeserializeOwned, Serialize};

use crate::{debug::BytesDebug, Frame, GgrsInput, NULL_FRAME};

/// Represents the game state of your game for a single frame. The `data` holds the game state, `frame` indicates the associated frame number
/// and `checksum` can additionally be provided for use during a `SyncTestSession`.
#[derive(Debug, Clone)]
pub(crate) struct GameState<S> {
    /// The frame to which this info belongs to.
    pub frame: Frame,
    /// The game state
    pub data: Option<S>,
    /// The checksum of the gamestate.
    pub checksum: Option<u128>,
}

impl<S> Default for GameState<S> {
    fn default() -> Self {
        Self {
            frame: NULL_FRAME,
            data: None,
            checksum: None,
        }
    }
}

/// Represents an input for a single player in a single frame. The associated frame is denoted with `frame`.
/// You do not need to create this struct, but the sessions will provide a `Vec<PlayerInput>` for you during `advance_frame()`.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct PlayerInput {
    /// The frame to which this info belongs to. -1/[`NULL_FRAME`] represents an invalid frame
    pub frame: Frame,
    /// The input struct given by the user
    pub input: SerializedPlayerInput,
}

impl PlayerInput {
    pub(crate) fn new_from_input(frame: Frame, input: impl GgrsInput) -> Self {
        Self {
            frame,
            input: SerializedPlayerInput::encode(input),
        }
    }

    pub(crate) fn new_blank_input<I: GgrsInput>(frame: Frame) -> Self {
        Self::new_from_input(frame, I::default())
    }

    pub(crate) fn new_from_bytes(frame: Frame, bytes: Vec<u8>) -> Self {
        Self {
            frame,
            input: SerializedPlayerInput(bytes),
        }
    }

    pub(crate) fn equal(&self, other: &Self, input_only: bool) -> bool {
        (input_only || self.frame == other.frame) && self.input == other.input
    }
}

#[derive(Clone, PartialEq)]
pub(crate) struct SerializedPlayerInput(Vec<u8>);

impl SerializedPlayerInput {
    pub(crate) fn encode<I: Serialize>(input: I) -> Self {
        Self(bincode::serialize(&input).expect("input serialization failed"))
    }

    pub(crate) fn decode<I: DeserializeOwned>(&self) -> I {
        bincode::deserialize(&self.0).expect("input deserialization failed")
    }

    pub(crate) fn as_slice(&self) -> &[u8] {
        &self.0
    }
}

impl std::fmt::Debug for SerializedPlayerInput {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{:?}", &BytesDebug(&self.0))
    }
}

// #########
// # TESTS #
// #########

#[cfg(test)]
mod game_input_tests {
    use serde::Deserialize;

    use super::*;

    #[repr(C)]
    #[derive(Default, Serialize, Deserialize)]
    struct TestInput {
        inp: u8,
    }

    #[test]
    fn test_input_equality() {
        let input1 = PlayerInput::new_from_input(0, TestInput { inp: 5 });
        let input2 = PlayerInput::new_from_input(0, TestInput { inp: 5 });
        assert!(input1.equal(&input2, false));
    }

    #[test]
    fn test_input_equality_input_only() {
        let input1 = PlayerInput::new_from_input(0, TestInput { inp: 5 });
        let input2 = PlayerInput::new_from_input(5, TestInput { inp: 5 });
        assert!(input1.equal(&input2, true)); // different frames, but does not matter
    }

    #[test]
    fn test_input_equality_fail() {
        let input1 = PlayerInput::new_from_input(0, TestInput { inp: 5 });
        let input2 = PlayerInput::new_from_input(0, TestInput { inp: 7 });
        assert!(!input1.equal(&input2, false)); // different bits
    }
}
