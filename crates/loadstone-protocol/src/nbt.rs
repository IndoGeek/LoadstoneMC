//! NBT encoder and decoder.
//!
//! The network protocol uses "anonymous NBT" (the root tag name is omitted) for
//! text components, registry entry data and chat. Region files and `level.dat`
//! use the ordinary named form, so both directions are supported here.

use crate::error::ProtocolError;
use crate::read::PacketReader;

/// A single NBT tag. Ordered compounds preserve insertion order, which the
/// game relies on for deterministic output.
#[derive(Debug, Clone, PartialEq)]
pub enum Nbt {
    Byte(i8),
    Short(i16),
    Int(i32),
    Long(i64),
    Float(f32),
    Double(f64),
    ByteArray(Vec<u8>),
    String(String),
    List(Vec<Nbt>),
    Compound(Vec<(String, Nbt)>),
    IntArray(Vec<i32>),
    LongArray(Vec<i64>),
}

impl Nbt {
    fn id(&self) -> u8 {
        match self {
            Nbt::Byte(_) => 0x01,
            Nbt::Short(_) => 0x02,
            Nbt::Int(_) => 0x03,
            Nbt::Long(_) => 0x04,
            Nbt::Float(_) => 0x05,
            Nbt::Double(_) => 0x06,
            Nbt::ByteArray(_) => 0x07,
            Nbt::String(_) => 0x08,
            Nbt::List(_) => 0x09,
            Nbt::Compound(_) => 0x0A,
            Nbt::IntArray(_) => 0x0B,
            Nbt::LongArray(_) => 0x0C,
        }
    }

    /// A `{"text": ...}` text component, the only component form the server
    /// currently emits.
    pub fn text(message: impl Into<String>) -> Nbt {
        Nbt::Compound(vec![("text".to_string(), Nbt::String(message.into()))])
    }

    pub fn compound(entries: impl IntoIterator<Item = (impl Into<String>, Nbt)>) -> Nbt {
        Nbt::Compound(entries.into_iter().map(|(k, v)| (k.into(), v)).collect())
    }

    fn write_payload(&self, out: &mut Vec<u8>) {
        match self {
            Nbt::Byte(v) => out.push(*v as u8),
            Nbt::Short(v) => out.extend_from_slice(&v.to_be_bytes()),
            Nbt::Int(v) => out.extend_from_slice(&v.to_be_bytes()),
            Nbt::Long(v) => out.extend_from_slice(&v.to_be_bytes()),
            Nbt::Float(v) => out.extend_from_slice(&v.to_be_bytes()),
            Nbt::Double(v) => out.extend_from_slice(&v.to_be_bytes()),
            Nbt::ByteArray(v) => {
                out.extend_from_slice(&(v.len() as i32).to_be_bytes());
                out.extend_from_slice(v);
            }
            Nbt::String(s) => {
                let bytes = s.as_bytes();
                out.extend_from_slice(&(bytes.len() as u16).to_be_bytes());
                out.extend_from_slice(bytes);
            }
            Nbt::List(items) => {
                // Empty lists have element type 0 (TAG_End).
                let element_id = items.first().map(Nbt::id).unwrap_or(0);
                out.push(element_id);
                out.extend_from_slice(&(items.len() as i32).to_be_bytes());
                for item in items {
                    item.write_payload(out);
                }
            }
            Nbt::Compound(entries) => {
                for (name, value) in entries {
                    out.push(value.id());
                    let bytes = name.as_bytes();
                    out.extend_from_slice(&(bytes.len() as u16).to_be_bytes());
                    out.extend_from_slice(bytes);
                    value.write_payload(out);
                }
                out.push(0x00);
            }
            Nbt::IntArray(v) => {
                out.extend_from_slice(&(v.len() as i32).to_be_bytes());
                for item in v {
                    out.extend_from_slice(&item.to_be_bytes());
                }
            }
            Nbt::LongArray(v) => {
                out.extend_from_slice(&(v.len() as i32).to_be_bytes());
                for item in v {
                    out.extend_from_slice(&item.to_be_bytes());
                }
            }
        }
    }

    /// Encode as "anonymous NBT": the root tag's name is omitted.
    pub fn to_anonymous_bytes(&self) -> Vec<u8> {
        let mut out = Vec::new();
        match self {
            Nbt::Compound(entries) => {
                out.push(0x0A);
                for (name, value) in entries {
                    out.push(value.id());
                    let bytes = name.as_bytes();
                    out.extend_from_slice(&(bytes.len() as u16).to_be_bytes());
                    out.extend_from_slice(bytes);
                    value.write_payload(&mut out);
                }
                out.push(0x00);
            }
            other => other.write_payload(&mut out),
        }
        out
    }

    /// Encode with a named root, the form used by region files and `level.dat`.
    pub fn to_named_bytes(&self, name: &str) -> Vec<u8> {
        let mut out = Vec::new();
        out.push(0x0A);
        write_name(&mut out, name);
        self.write_payload(&mut out);
        out
    }

    /// Read a named root tag (`[type][name][payload]`) from a byte slice.
    pub fn read_root(reader: &mut PacketReader) -> Result<(String, Nbt), ProtocolError> {
        let ty = reader.read_u8()?;
        if ty == 0 {
            return Err(ProtocolError::InvalidEnum(0));
        }
        let name = read_name(reader)?;
        Ok((name, read_payload(reader, ty)?))
    }

    /// Read a single anonymous tag (`[type][payload]`, no root name).
    pub fn read_anonymous(reader: &mut PacketReader) -> Result<Nbt, ProtocolError> {
        let ty = reader.read_u8()?;
        read_payload(reader, ty)
    }
}

fn write_name(out: &mut Vec<u8>, name: &str) {
    let bytes = name.as_bytes();
    out.extend_from_slice(&(bytes.len() as u16).to_be_bytes());
    out.extend_from_slice(bytes);
}

fn read_name(reader: &mut PacketReader) -> Result<String, ProtocolError> {
    let len = reader.read_u16()? as usize;
    let bytes = reader.read_bytes(len)?;
    std::str::from_utf8(bytes)
        .map(str::to_string)
        .map_err(|_| ProtocolError::InvalidUtf8)
}

fn read_payload(reader: &mut PacketReader, ty: u8) -> Result<Nbt, ProtocolError> {
    Ok(match ty {
        0x01 => Nbt::Byte(reader.read_i8()?),
        0x02 => Nbt::Short(reader.read_i16()?),
        0x03 => Nbt::Int(reader.read_i32()?),
        0x04 => Nbt::Long(reader.read_i64()?),
        0x05 => Nbt::Float(reader.read_f32()?),
        0x06 => Nbt::Double(reader.read_f64()?),
        0x07 => {
            let len = non_negative(reader.read_i32()?)?;
            Nbt::ByteArray(reader.read_bytes(len)?.to_vec())
        }
        0x08 => Nbt::String(read_name(reader)?),
        0x09 => {
            let element_ty = reader.read_u8()?;
            let len = non_negative(reader.read_i32()?)?;
            let mut items = Vec::with_capacity(len);
            for _ in 0..len {
                items.push(read_payload(reader, element_ty)?);
            }
            Nbt::List(items)
        }
        0x0A => {
            let mut entries = Vec::new();
            loop {
                let entry_ty = reader.read_u8()?;
                if entry_ty == 0 {
                    break;
                }
                let name = read_name(reader)?;
                entries.push((name, read_payload(reader, entry_ty)?));
            }
            Nbt::Compound(entries)
        }
        0x0B => {
            let len = non_negative(reader.read_i32()?)?;
            let mut items = Vec::with_capacity(len);
            for _ in 0..len {
                items.push(reader.read_i32()?);
            }
            Nbt::IntArray(items)
        }
        0x0C => {
            let len = non_negative(reader.read_i32()?)?;
            let mut items = Vec::with_capacity(len);
            for _ in 0..len {
                items.push(reader.read_i64()?);
            }
            Nbt::LongArray(items)
        }
        other => return Err(ProtocolError::InvalidEnum(i32::from(other))),
    })
}

fn non_negative(len: i32) -> Result<usize, ProtocolError> {
    if len < 0 {
        return Err(ProtocolError::InvalidStringLength(len));
    }
    Ok(len as usize)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn encodes_named_compound() {
        let nbt = Nbt::compound([("name", Nbt::String("x".into())), ("count", Nbt::Int(5))]);
        let bytes = nbt.to_anonymous_bytes();
        assert_eq!(
            bytes,
            vec![
                0x0A, // root compound
                0x08, 0x00, 0x04, b'n', b'a', b'm', b'e', // string "name"
                0x00, 0x01, b'x', // value "x"
                0x03, 0x00, 0x05, b'c', b'o', b'u', b'n', b't', // int "count"
                0x00, 0x00, 0x00, 0x05, // value 5
                0x00, // end
            ]
        );
    }

    #[test]
    fn text_component_has_no_root_name() {
        let bytes = Nbt::text("hi").to_anonymous_bytes();
        assert_eq!(bytes[0], 0x0A);
        assert_eq!(&bytes[1..3], &[0x08, 0x00]);
    }

    #[test]
    fn named_roundtrip_preserves_nested_tags() {
        let nbt = Nbt::compound([
            ("DataVersion", Nbt::Int(4671)),
            ("Name", Nbt::String("minecraft:grass_block".into())),
            (
                "Properties",
                Nbt::compound([("snowy", Nbt::String("false".into()))]),
            ),
            (
                "Heightmaps",
                Nbt::compound([("WORLD_SURFACE", Nbt::LongArray(vec![1, 2, 3]))]),
            ),
            ("block_entities", Nbt::List(vec![])),
        ]);
        let bytes = nbt.to_named_bytes("chunk");
        let mut reader = crate::read::PacketReader::new(&bytes);
        let (name, decoded) = Nbt::read_root(&mut reader).unwrap();
        assert_eq!(name, "chunk");
        assert_eq!(decoded, nbt);
        assert!(reader.is_empty());
    }

    #[test]
    fn anonymous_roundtrip_matches_text_component() {
        let nbt = Nbt::text("hello");
        let bytes = nbt.to_anonymous_bytes();
        let mut reader = crate::read::PacketReader::new(&bytes);
        assert_eq!(Nbt::read_anonymous(&mut reader).unwrap(), nbt);
    }

    #[test]
    fn empty_list_uses_end_element_type() {
        let nbt = Nbt::compound([("l", Nbt::List(vec![]))]);
        let bytes = nbt.to_anonymous_bytes();
        // 0x09 tag, "l", element type 0, i32 length 0, then the compound end.
        assert_eq!(&bytes[1..4], &[0x09, 0x00, 0x01]);
        assert_eq!(&bytes[4..5], b"l");
        assert_eq!(&bytes[5..10], &[0x00, 0x00, 0x00, 0x00, 0x00]);
        assert_eq!(bytes[10], 0x00);
    }
}
