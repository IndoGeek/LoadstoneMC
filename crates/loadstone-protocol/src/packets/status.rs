use crate::error::ProtocolError;
use crate::packet::Packet;
use crate::read::PacketReader;
use crate::write::PacketWriter;
use serde::{Deserialize, Serialize};

/// C->S Status Request (id 0x00).
#[derive(Debug, Clone)]
pub struct StatusRequest;

impl Packet for StatusRequest {
    const ID: i32 = 0x00;

    fn encode(&self, _out: &mut PacketWriter) {}

    fn decode(_reader: &mut PacketReader<'_>) -> Result<Self, ProtocolError> {
        Ok(StatusRequest)
    }
}

/// S->C Status Response (id 0x00) carrying the server list JSON.
#[derive(Debug, Clone)]
pub struct StatusResponse {
    pub json: String,
}

impl Packet for StatusResponse {
    const ID: i32 = 0x00;

    fn encode(&self, out: &mut PacketWriter) {
        out.write_string(&self.json);
    }

    fn decode(reader: &mut PacketReader<'_>) -> Result<Self, ProtocolError> {
        Ok(Self {
            json: reader.read_string()?.to_string(),
        })
    }
}

/// C->S Ping (id 0x01).
#[derive(Debug, Clone)]
pub struct PingRequest {
    pub payload: i64,
}

impl Packet for PingRequest {
    const ID: i32 = 0x01;

    fn encode(&self, out: &mut PacketWriter) {
        out.write_i64(self.payload);
    }

    fn decode(reader: &mut PacketReader<'_>) -> Result<Self, ProtocolError> {
        Ok(Self {
            payload: reader.read_i64()?,
        })
    }
}

/// S->C Pong (id 0x01), echoes the ping payload.
pub type PongResponse = PingRequest;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StatusVersion {
    pub name: String,
    pub protocol: i32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StatusPlayers {
    pub max: i32,
    pub online: i32,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub sample: Vec<StatusPlayerSample>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StatusPlayerSample {
    pub name: String,
    pub id: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StatusDescription {
    pub text: String,
}

/// The JSON body returned in [`StatusResponse`].
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ServerListStatus {
    pub version: StatusVersion,
    pub players: StatusPlayers,
    pub description: StatusDescription,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub favicon: Option<String>,
    #[serde(rename = "enforcesSecureChat", default)]
    pub enforces_secure_chat: bool,
}
