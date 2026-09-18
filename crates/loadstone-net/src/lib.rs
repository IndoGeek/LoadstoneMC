pub mod auth;
mod codec;
mod connection;
mod crypto;
mod error;

pub use codec::{read_frame, write_frame, FrameError};
pub use connection::{
    run_connection, Connection, ConnectionConfig, RawPacket, COMPRESSION_THRESHOLD,
};
pub use crypto::{Cfb8, EncryptedStream};
pub use error::NetError;
