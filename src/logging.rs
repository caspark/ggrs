#[cfg(not(any(feature = "logging-via-log", feature = "logging-via-tracing")))]
#[macro_use]
mod logging_impl {
    /// Internal no-op macro for logging at trace level.
    #[macro_export]
    macro_rules! trace {
        ($($arg:tt)*) => {};
    }

    /// Internal no-op macro for logging at debug level.
    #[macro_export]
    macro_rules! debug {
        ($($arg:tt)*) => {};
    }

    /// Internal no-op macro for logging at info level.
    #[macro_export]
    macro_rules! info {
        ($($arg:tt)*) => {};
    }

    /// Internal no-op macro for logging at warn level.
    #[macro_export]
    macro_rules! warn {
        ($($arg:tt)*) => {};
    }

    /// Internal no-op macro for logging at error level.
    #[macro_export]
    macro_rules! error {
        ($($arg:tt)*) => {};
    }

    pub(crate) use crate::{debug, error, info, trace, warn};
}

#[cfg(not(any(feature = "logging-via-log", feature = "logging-via-tracing")))]
pub(crate) use logging_impl::*;

#[cfg(feature = "logging-via-log")]
pub(crate) use log::{debug, error, info, trace, warn};

#[cfg(feature = "logging-via-tracing")]
pub(crate) use tracing::{debug, error, info, trace, warn};
