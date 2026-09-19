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
