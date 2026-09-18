use thiserror::Error;

#[derive(Debug, Error)]
pub enum ProtocolError {
    #[error("varint is too big (more than 5 bytes)")]
    VarIntTooBig,
    #[error("unexpected end of data: needed {needed} bytes, had {had}")]
    UnexpectedEof { needed: usize, had: usize },
    #[error("invalid string length: {0}")]
    InvalidStringLength(i32),
    #[error("invalid utf-8 in string")]
    InvalidUtf8,
    #[error("invalid boolean value: {0}")]
    InvalidBool(u8),
    #[error("invalid enum discriminant: {0}")]
    InvalidEnum(i32),
    #[error("unknown packet id {id:#x} in state {state}")]
    UnknownPacketId { state: &'static str, id: i32 },
    #[error("json error: {0}")]
    Json(#[from] serde_json::Error),
}
