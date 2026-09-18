use crate::error::ProtocolError;
use crate::read::PacketReader;
use crate::write::PacketWriter;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct PacketId(pub i32);

/// Anything that can be encoded into a full frame
/// (`VarInt length | VarInt packet id | payload`).
pub trait Packet: Sized {
    const ID: i32;

    fn encode(&self, out: &mut PacketWriter);

    fn decode(reader: &mut PacketReader<'_>) -> Result<Self, ProtocolError>;

    /// Encode with length prefix and packet id, ready for the socket.
    fn to_frame(&self) -> Vec<u8> {
        let mut body = PacketWriter::with_capacity(64);
        body.write_varint(Self::ID);
        self.encode(&mut body);

        let mut frame = Vec::with_capacity(body.len() + 5);
        crate::write_varint(&mut frame, body.len() as i32);
        frame.extend_from_slice(body.as_slice());
        frame
    }
}
