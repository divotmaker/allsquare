//! Error types.

/// Errors produced by this crate.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum Error {
    /// A notification was shorter than its type requires.
    #[error("truncated {kind} frame: {len} bytes, need at least {need}")]
    Truncated {
        /// Frame kind being parsed.
        kind: &'static str,
        /// Bytes actually present.
        len: usize,
        /// Bytes required.
        need: usize,
    },

    /// The first byte did not identify a known message family.
    ///
    /// Note that `0x11` is *not* a universal prefix: the device also emits
    /// `0x91` (battery) and `0x71` (clock) unprompted.
    #[error("unknown message family 0x{0:02x}")]
    UnknownFamily(u8),

    /// A `0x11`-family notification with an unrecognized type byte.
    #[error("unknown notification type 0x{0:02x}")]
    UnknownNotification(u8),

    /// The device closed the link. A zero-length notification means this.
    #[error("device closed the connection")]
    Disconnected,

    /// The transport failed.
    #[error("transport: {0}")]
    Transport(#[from] std::io::Error),

    /// The device could not be found while scanning.
    #[error("no Square Golf device found")]
    NotFound,

    /// Backend-specific failure that has no better representation.
    #[error("{0}")]
    Backend(String),
}

/// Convenience alias.
pub type Result<T> = core::result::Result<T, Error>;
