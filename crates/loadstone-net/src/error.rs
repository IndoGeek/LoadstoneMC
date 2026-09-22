use std::io;

use thiserror::Error;

#[derive(Debug, Error)]
pub enum NetError {
    #[error(transparent)]
    Frame(#[from] crate::codec::FrameError),
    #[error("protocol error: {0}")]
    Protocol(#[from] loadstone_protocol::ProtocolError),
    #[error("connection closed by peer")]
    Closed,
    #[error("json error: {0}")]
    Json(#[from] serde_json::Error),
    #[error("already running encryption on this connection")]
    AlreadyEncrypted,
    #[error("decrypted shared secret has invalid length {0} (expected 16)")]
    InvalidSharedSecret(usize),
    #[error("encryption response did not echo the verify token sent to the client")]
    InvalidVerifyToken,
    #[error("authentication error: {0}")]
    Auth(String),
    #[error("internal server error: {0}")]
    Internal(String),
}

/// True for the socket errors a departing client produces: it closed the
/// connection, or the kernel reported that the peer is gone. These are ordinary
/// events (vanilla logs `lost connection: Disconnected`), not server faults, and
/// the caller should not report them as errors.
pub(crate) fn is_disconnect_kind(e: &io::Error) -> bool {
    matches!(
        e.kind(),
        io::ErrorKind::UnexpectedEof
            | io::ErrorKind::ConnectionReset
            | io::ErrorKind::ConnectionAborted
            | io::ErrorKind::BrokenPipe
    )
}

impl NetError {
    /// Whether this error is just the client going away rather than a fault on
    /// our side.
    pub fn is_client_disconnect(&self) -> bool {
        match self {
            NetError::Closed => true,
            NetError::Frame(crate::codec::FrameError::Io(e)) => is_disconnect_kind(e),
            _ => false,
        }
    }
}
