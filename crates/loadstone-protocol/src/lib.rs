mod error;
mod nbt;
mod packet;
mod read;
mod varint;
mod write;

pub mod packets;

pub use error::ProtocolError;
pub use nbt::Nbt;
pub use packet::{Packet, PacketId};
pub use read::PacketReader;
pub use varint::{read_varint, write_varint, VarInt};
pub use write::PacketWriter;

/// Target Minecraft version this protocol implementation speaks.
pub const PROTOCOL_VERSION: i32 = 774;
pub const MINECRAFT_VERSION: &str = "1.21.11";
