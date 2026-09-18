use crate::error::ProtocolError;
use crate::packet::Packet;
use crate::read::PacketReader;
use crate::write::PacketWriter;

/// C->S handshake, always packet id 0x00 in the Handshake state.
#[derive(Debug, Clone)]
pub struct Handshake {
    pub protocol_version: i32,
    pub server_address: String,
    pub server_port: u16,
    pub next_state: i32,
}

impl Packet for Handshake {
    const ID: i32 = 0x00;

    fn encode(&self, out: &mut PacketWriter) {
        out.write_varint(self.protocol_version)
            .write_string(&self.server_address)
            .write_i16(self.server_port as i16)
            .write_varint(self.next_state);
    }

    fn decode(reader: &mut PacketReader<'_>) -> Result<Self, ProtocolError> {
        Ok(Self {
            protocol_version: reader.read_varint()?,
            server_address: reader.read_string()?.to_string(),
            server_port: reader.read_u16()?,
            next_state: reader.read_varint()?,
        })
    }
}
