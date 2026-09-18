use crate::error::ProtocolError;
use crate::packet::Packet;
use crate::read::PacketReader;
use crate::write::PacketWriter;
use uuid::Uuid;

/// C->S Login Start (1.21.x, id 0x00).
#[derive(Debug, Clone)]
pub struct LoginStart {
    pub name: String,
    pub uuid: Uuid,
}

impl Packet for LoginStart {
    const ID: i32 = 0x00;

    fn encode(&self, out: &mut PacketWriter) {
        out.write_string(&self.name).write_uuid(self.uuid);
    }

    fn decode(reader: &mut PacketReader<'_>) -> Result<Self, ProtocolError> {
        Ok(Self {
            name: reader.read_string()?.to_string(),
            uuid: reader.read_uuid()?,
        })
    }
}

/// S->C Login Disconnect (id 0x00).
#[derive(Debug, Clone)]
pub struct LoginDisconnect {
    /// JSON-encoded text component describing the reason.
    pub reason: String,
}

impl Packet for LoginDisconnect {
    const ID: i32 = 0x00;

    fn encode(&self, out: &mut PacketWriter) {
        out.write_string(&self.reason);
    }

    fn decode(reader: &mut PacketReader<'_>) -> Result<Self, ProtocolError> {
        Ok(Self {
            reason: reader.read_string()?.to_string(),
        })
    }
}

/// S->C Encryption Request (id 0x01). `public_key` and `verify_token` are
/// VarInt-length-prefixed byte buffers.
#[derive(Debug, Clone)]
pub struct EncryptionRequest {
    pub server_id: String,
    pub public_key: Vec<u8>,
    pub verify_token: Vec<u8>,
    pub should_authenticate: bool,
}

impl Packet for EncryptionRequest {
    const ID: i32 = 0x01;

    fn encode(&self, out: &mut PacketWriter) {
        out.write_string(&self.server_id)
            .write_varint(self.public_key.len() as i32)
            .write_bytes(&self.public_key)
            .write_varint(self.verify_token.len() as i32)
            .write_bytes(&self.verify_token)
            .write_bool(self.should_authenticate);
    }

    fn decode(reader: &mut PacketReader<'_>) -> Result<Self, ProtocolError> {
        let server_id = reader.read_string()?.to_string();
        let key_len = reader.read_varint()?;
        let public_key = reader.read_bytes(key_len as usize)?.to_vec();
        let token_len = reader.read_varint()?;
        let verify_token = reader.read_bytes(token_len as usize)?.to_vec();
        let should_authenticate = reader.read_bool()?;
        Ok(Self {
            server_id,
            public_key,
            verify_token,
            should_authenticate,
        })
    }
}

/// C->S Encryption Response (id 0x01).
#[derive(Debug, Clone)]
pub struct EncryptionResponse {
    /// Shared secret encrypted with the server's RSA public key.
    pub shared_secret: Vec<u8>,
    /// The server's verify token, signed with the player's account key.
    pub verify_token: Vec<u8>,
}

impl Packet for EncryptionResponse {
    const ID: i32 = 0x01;

    fn encode(&self, out: &mut PacketWriter) {
        out.write_varint(self.shared_secret.len() as i32)
            .write_bytes(&self.shared_secret)
            .write_varint(self.verify_token.len() as i32)
            .write_bytes(&self.verify_token);
    }

    fn decode(reader: &mut PacketReader<'_>) -> Result<Self, ProtocolError> {
        let secret_len = reader.read_varint()?;
        let shared_secret = reader.read_bytes(secret_len as usize)?.to_vec();
        let token_len = reader.read_varint()?;
        let verify_token = reader.read_bytes(token_len as usize)?.to_vec();
        Ok(Self {
            shared_secret,
            verify_token,
        })
    }
}

/// S->C Login Success (id 0x02).
#[derive(Debug, Clone)]
pub struct LoginSuccess {
    pub uuid: Uuid,
    pub username: String,
    pub properties: Vec<GameProfileProperty>,
}

#[derive(Debug, Clone)]
pub struct GameProfileProperty {
    pub name: String,
    pub value: String,
    pub signature: Option<String>,
}

impl Packet for LoginSuccess {
    const ID: i32 = 0x02;

    fn encode(&self, out: &mut PacketWriter) {
        out.write_uuid(self.uuid).write_string(&self.username);
        out.write_varint(self.properties.len() as i32);
        for property in &self.properties {
            out.write_string(&property.name)
                .write_string(&property.value);
            match &property.signature {
                Some(sig) => {
                    out.write_bool(true).write_string(sig);
                }
                None => {
                    out.write_bool(false);
                }
            }
        }
    }

    fn decode(reader: &mut PacketReader<'_>) -> Result<Self, ProtocolError> {
        let uuid = reader.read_uuid()?;
        let username = reader.read_string()?.to_string();
        let count = reader.read_varint()? as usize;
        let mut properties = Vec::with_capacity(count);
        for _ in 0..count {
            let name = reader.read_string()?.to_string();
            let value = reader.read_string()?.to_string();
            let signature = if reader.read_bool()? {
                Some(reader.read_string()?.to_string())
            } else {
                None
            };
            properties.push(GameProfileProperty {
                name,
                value,
                signature,
            });
        }
        Ok(Self {
            uuid,
            username,
            properties,
        })
    }
}

/// S->C Set Compression (id 0x03).
#[derive(Debug, Clone, Copy)]
pub struct SetCompression {
    pub threshold: i32,
}

impl Packet for SetCompression {
    const ID: i32 = 0x03;

    fn encode(&self, out: &mut PacketWriter) {
        out.write_varint(self.threshold);
    }

    fn decode(reader: &mut PacketReader<'_>) -> Result<Self, ProtocolError> {
        Ok(Self {
            threshold: reader.read_varint()?,
        })
    }
}

/// C->S Login Acknowledged (id 0x03): client has received Login Success and
/// transitions to the Configuration state.
#[derive(Debug, Clone, Copy)]
pub struct LoginAcknowledged;

impl Packet for LoginAcknowledged {
    const ID: i32 = 0x03;

    fn encode(&self, _out: &mut PacketWriter) {}

    fn decode(_reader: &mut PacketReader<'_>) -> Result<Self, ProtocolError> {
        Ok(LoginAcknowledged)
    }
}
