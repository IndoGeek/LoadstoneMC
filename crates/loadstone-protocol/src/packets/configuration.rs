//! Configuration-state packets for protocol 774 (1.21.11).
//!
//! Packet ids are per *state and direction*: `0x03` is clientbound
//! `finish_configuration` and serverbound `finish_configuration` (the
//! acknowledgement) at the same time. The trait's `ID` is only unique within
//! one direction, which is why the serverbound types are separate structs.

use crate::error::ProtocolError;
use crate::packet::Packet;
use crate::read::PacketReader;
use crate::write::PacketWriter;

/// A data pack announced during known-pack negotiation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KnownPack {
    pub namespace: String,
    pub id: String,
    pub version: String,
}

fn write_known_packs(out: &mut PacketWriter, packs: &[KnownPack]) {
    out.write_varint(packs.len() as i32);
    for pack in packs {
        out.write_string(&pack.namespace)
            .write_string(&pack.id)
            .write_string(&pack.version);
    }
}

fn read_known_packs(reader: &mut PacketReader<'_>) -> Result<Vec<KnownPack>, ProtocolError> {
    let count = reader.read_varint()?;
    if count < 0 {
        return Err(ProtocolError::InvalidStringLength(count));
    }
    let mut packs = Vec::with_capacity(count as usize);
    for _ in 0..count {
        packs.push(KnownPack {
            namespace: reader.read_string()?.to_string(),
            id: reader.read_string()?.to_string(),
            version: reader.read_string()?.to_string(),
        });
    }
    Ok(packs)
}

fn read_string_list(reader: &mut PacketReader<'_>) -> Result<Vec<String>, ProtocolError> {
    let count = reader.read_varint()?;
    if count < 0 {
        return Err(ProtocolError::InvalidStringLength(count));
    }
    let mut items = Vec::with_capacity(count as usize);
    for _ in 0..count {
        items.push(reader.read_string()?.to_string());
    }
    Ok(items)
}

/// S->C Custom Payload (id 0x01). Used for the `minecraft:brand` message.
#[derive(Debug, Clone)]
pub struct CustomPayload {
    pub channel: String,
    pub data: Vec<u8>,
}

impl Packet for CustomPayload {
    const ID: i32 = 0x01;

    fn encode(&self, out: &mut PacketWriter) {
        out.write_string(&self.channel)
            .write_varint(self.data.len() as i32)
            .write_bytes(&self.data);
    }

    fn decode(reader: &mut PacketReader<'_>) -> Result<Self, ProtocolError> {
        let channel = reader.read_string()?.to_string();
        let len = reader.read_varint()?;
        if len < 0 {
            return Err(ProtocolError::InvalidStringLength(len));
        }
        let data = reader.read_bytes(len as usize)?.to_vec();
        Ok(Self { channel, data })
    }
}

/// S->C Disconnect (id 0x02). The reason is anonymous NBT, unlike the login
/// state where it is a JSON string.
#[derive(Debug, Clone)]
pub struct ConfigurationDisconnect {
    pub reason: Vec<u8>,
}

impl Packet for ConfigurationDisconnect {
    const ID: i32 = 0x02;

    fn encode(&self, out: &mut PacketWriter) {
        out.write_bytes(&self.reason);
    }

    fn decode(reader: &mut PacketReader<'_>) -> Result<Self, ProtocolError> {
        Ok(Self {
            reason: reader.read_rest().to_vec(),
        })
    }
}

/// S->C Finish Configuration (id 0x03).
#[derive(Debug, Clone, Copy)]
pub struct FinishConfiguration;

impl Packet for FinishConfiguration {
    const ID: i32 = 0x03;

    fn encode(&self, _out: &mut PacketWriter) {}

    fn decode(_reader: &mut PacketReader<'_>) -> Result<Self, ProtocolError> {
        Ok(FinishConfiguration)
    }
}

/// S->C Keep Alive (id 0x04).
#[derive(Debug, Clone, Copy)]
pub struct ConfigurationKeepAlive {
    pub id: i64,
}

impl Packet for ConfigurationKeepAlive {
    const ID: i32 = 0x04;

    fn encode(&self, out: &mut PacketWriter) {
        out.write_i64(self.id);
    }

    fn decode(reader: &mut PacketReader<'_>) -> Result<Self, ProtocolError> {
        Ok(Self {
            id: reader.read_i64()?,
        })
    }
}

/// S->C Ping (id 0x05).
#[derive(Debug, Clone, Copy)]
pub struct ConfigurationPing {
    pub id: i32,
}

impl Packet for ConfigurationPing {
    const ID: i32 = 0x05;

    fn encode(&self, out: &mut PacketWriter) {
        out.write_i32(self.id);
    }

    fn decode(reader: &mut PacketReader<'_>) -> Result<Self, ProtocolError> {
        Ok(Self {
            id: reader.read_i32()?,
        })
    }
}

/// One entry of a registry. `data` is the entry's NBT; when `None` the client
/// resolves it from a negotiated known pack.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RegistryEntry {
    pub name: String,
    pub data: Option<Vec<u8>>,
}

/// S->C Registry Data (id 0x07).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RegistryData {
    pub registry_id: String,
    pub entries: Vec<RegistryEntry>,
}

impl Packet for RegistryData {
    const ID: i32 = 0x07;

    fn encode(&self, out: &mut PacketWriter) {
        out.write_string(&self.registry_id);
        out.write_varint(self.entries.len() as i32);
        for entry in &self.entries {
            out.write_string(&entry.name);
            match &entry.data {
                Some(data) => {
                    out.write_bool(true).write_bytes(data);
                }
                None => {
                    out.write_bool(false);
                }
            }
        }
    }

    fn decode(reader: &mut PacketReader<'_>) -> Result<Self, ProtocolError> {
        let registry_id = reader.read_string()?.to_string();
        let count = reader.read_varint()?;
        if count < 0 {
            return Err(ProtocolError::InvalidStringLength(count));
        }
        let mut entries = Vec::with_capacity(count as usize);
        for _ in 0..count {
            let name = reader.read_string()?.to_string();
            let data = if reader.read_bool()? {
                Some(reader.read_rest().to_vec())
            } else {
                None
            };
            entries.push(RegistryEntry { name, data });
        }
        Ok(Self {
            registry_id,
            entries,
        })
    }
}

/// S->C Feature Flags (id 0x0C).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FeatureFlags {
    pub features: Vec<String>,
}

impl Packet for FeatureFlags {
    const ID: i32 = 0x0C;

    fn encode(&self, out: &mut PacketWriter) {
        out.write_varint(self.features.len() as i32);
        for feature in &self.features {
            out.write_string(feature);
        }
    }

    fn decode(reader: &mut PacketReader<'_>) -> Result<Self, ProtocolError> {
        Ok(Self {
            features: read_string_list(reader)?,
        })
    }
}

/// A single tag: a named set of registry entry ids.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NetworkTag {
    pub name: String,
    pub entries: Vec<i32>,
}

/// All tags belonging to one registry.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TagRegistry {
    pub registry: String,
    pub tags: Vec<NetworkTag>,
}

/// S->C Update Tags (id 0x0D). Tags are never sourced from known packs, so the
/// server must always send the complete set.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct UpdateTags {
    pub registries: Vec<TagRegistry>,
}

impl Packet for UpdateTags {
    const ID: i32 = 0x0D;

    fn encode(&self, out: &mut PacketWriter) {
        out.write_varint(self.registries.len() as i32);
        for registry in &self.registries {
            out.write_string(&registry.registry);
            out.write_varint(registry.tags.len() as i32);
            for tag in &registry.tags {
                out.write_string(&tag.name);
                out.write_varint(tag.entries.len() as i32);
                for id in &tag.entries {
                    out.write_varint(*id);
                }
            }
        }
    }

    fn decode(reader: &mut PacketReader<'_>) -> Result<Self, ProtocolError> {
        let count = reader.read_varint()?;
        if count < 0 {
            return Err(ProtocolError::InvalidStringLength(count));
        }
        let mut registries = Vec::with_capacity(count as usize);
        for _ in 0..count {
            let registry = reader.read_string()?.to_string();
            let tag_count = reader.read_varint()?;
            if tag_count < 0 {
                return Err(ProtocolError::InvalidStringLength(tag_count));
            }
            let mut tags = Vec::with_capacity(tag_count as usize);
            for _ in 0..tag_count {
                let name = reader.read_string()?.to_string();
                let entry_count = reader.read_varint()?;
                if entry_count < 0 {
                    return Err(ProtocolError::InvalidStringLength(entry_count));
                }
                let mut entries = Vec::with_capacity(entry_count as usize);
                for _ in 0..entry_count {
                    entries.push(reader.read_varint()?);
                }
                tags.push(NetworkTag { name, entries });
            }
            registries.push(TagRegistry { registry, tags });
        }
        Ok(Self { registries })
    }
}

/// S->C Select Known Packs (id 0x0E).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClientboundKnownPacks {
    pub packs: Vec<KnownPack>,
}

impl Packet for ClientboundKnownPacks {
    const ID: i32 = 0x0E;

    fn encode(&self, out: &mut PacketWriter) {
        write_known_packs(out, &self.packs);
    }

    fn decode(reader: &mut PacketReader<'_>) -> Result<Self, ProtocolError> {
        Ok(Self {
            packs: read_known_packs(reader)?,
        })
    }
}

/// C->S Client Information (id 0x00).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClientInformation {
    pub locale: String,
    pub view_distance: i8,
    pub chat_flags: i32,
    pub chat_colors: bool,
    pub skin_parts: u8,
    pub main_hand: i32,
    pub text_filtering: bool,
    pub server_listing: bool,
    pub particle_status: i32,
}

impl Packet for ClientInformation {
    const ID: i32 = 0x00;

    fn encode(&self, out: &mut PacketWriter) {
        out.write_string(&self.locale)
            .write_i8(self.view_distance)
            .write_varint(self.chat_flags)
            .write_bool(self.chat_colors)
            .write_u8(self.skin_parts)
            .write_varint(self.main_hand)
            .write_bool(self.text_filtering)
            .write_bool(self.server_listing)
            .write_varint(self.particle_status);
    }

    fn decode(reader: &mut PacketReader<'_>) -> Result<Self, ProtocolError> {
        Ok(Self {
            locale: reader.read_string()?.to_string(),
            view_distance: reader.read_i8()?,
            chat_flags: reader.read_varint()?,
            chat_colors: reader.read_bool()?,
            skin_parts: reader.read_u8()?,
            main_hand: reader.read_varint()?,
            text_filtering: reader.read_bool()?,
            server_listing: reader.read_bool()?,
            particle_status: reader.read_varint()?,
        })
    }
}

/// C->S Select Known Packs (id 0x07).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ServerboundKnownPacks {
    pub packs: Vec<KnownPack>,
}

impl Packet for ServerboundKnownPacks {
    const ID: i32 = 0x07;

    fn encode(&self, out: &mut PacketWriter) {
        write_known_packs(out, &self.packs);
    }

    fn decode(reader: &mut PacketReader<'_>) -> Result<Self, ProtocolError> {
        Ok(Self {
            packs: read_known_packs(reader)?,
        })
    }
}

/// C->S Finish Configuration acknowledgement (id 0x03).
#[derive(Debug, Clone, Copy)]
pub struct AcknowledgeFinishConfiguration;

impl Packet for AcknowledgeFinishConfiguration {
    const ID: i32 = 0x03;

    fn encode(&self, _out: &mut PacketWriter) {}

    fn decode(_reader: &mut PacketReader<'_>) -> Result<Self, ProtocolError> {
        Ok(AcknowledgeFinishConfiguration)
    }
}

/// C->S Keep Alive (id 0x04).
#[derive(Debug, Clone, Copy)]
pub struct ConfigurationKeepAliveResponse {
    pub id: i64,
}

impl Packet for ConfigurationKeepAliveResponse {
    const ID: i32 = 0x04;

    fn encode(&self, out: &mut PacketWriter) {
        out.write_i64(self.id);
    }

    fn decode(reader: &mut PacketReader<'_>) -> Result<Self, ProtocolError> {
        Ok(Self {
            id: reader.read_i64()?,
        })
    }
}

/// C->S Pong (id 0x05).
#[derive(Debug, Clone, Copy)]
pub struct ConfigurationPong {
    pub id: i32,
}

impl Packet for ConfigurationPong {
    const ID: i32 = 0x05;

    fn encode(&self, out: &mut PacketWriter) {
        out.write_i32(self.id);
    }

    fn decode(reader: &mut PacketReader<'_>) -> Result<Self, ProtocolError> {
        Ok(Self {
            id: reader.read_i32()?,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn roundtrip<P: Packet + PartialEq + std::fmt::Debug>(packet: P) {
        let mut writer = PacketWriter::new();
        packet.encode(&mut writer);
        let bytes = writer.into_vec();
        let decoded = P::decode(&mut PacketReader::new(&bytes)).unwrap();
        assert_eq!(packet, decoded);
    }

    #[test]
    fn known_packs_roundtrip() {
        roundtrip(ClientboundKnownPacks {
            packs: vec![KnownPack {
                namespace: "minecraft".into(),
                id: "core".into(),
                version: "1.21.11".into(),
            }],
        });
        roundtrip(ServerboundKnownPacks { packs: vec![] });
    }

    #[test]
    fn registry_data_roundtrip_with_and_without_nbt() {
        let packet = RegistryData {
            registry_id: "minecraft:dimension_type".into(),
            entries: vec![
                RegistryEntry {
                    name: "minecraft:overworld".into(),
                    data: None,
                },
                RegistryEntry {
                    name: "minecraft:the_nether".into(),
                    data: Some(vec![0x0A, 0x00]),
                },
            ],
        };
        let mut writer = PacketWriter::new();
        packet.encode(&mut writer);
        let bytes = writer.into_vec();

        // The first entry is encoded as absent, the second as present.
        let decoded = RegistryData::decode(&mut PacketReader::new(&bytes)).unwrap();
        assert_eq!(decoded.entries[0].data, None);
        assert_eq!(decoded.entries[1].data.as_deref(), Some(&[0x0A, 0x00][..]));
        assert_eq!(packet, decoded);
    }

    #[test]
    fn tags_roundtrip() {
        roundtrip(UpdateTags {
            registries: vec![TagRegistry {
                registry: "minecraft:block".into(),
                tags: vec![NetworkTag {
                    name: "minecraft:mineable/pickaxe".into(),
                    entries: vec![1, 2, 300],
                }],
            }],
        });
    }

    #[test]
    fn client_information_is_parsed_in_order() {
        let info = ClientInformation {
            locale: "en_us".into(),
            view_distance: 12,
            chat_flags: 0,
            chat_colors: true,
            skin_parts: 0x7f,
            main_hand: 1,
            text_filtering: false,
            server_listing: true,
            particle_status: 0,
        };
        roundtrip(info);
    }
}
