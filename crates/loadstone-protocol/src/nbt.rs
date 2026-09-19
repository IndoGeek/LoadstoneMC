//! Minimal NBT encoder.
//!
//! Only serialization is implemented: the network protocol needs NBT for text
//! components (Configuration `Disconnect`), registry entry data, and later for
//! chat. The root tag name is omitted, matching the "anonymous NBT" form the
//! protocol uses on the wire.

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
