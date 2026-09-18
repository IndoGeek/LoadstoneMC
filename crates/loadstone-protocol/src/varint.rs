use crate::error::ProtocolError;

/// Minecraft VarInt: 7 bits per byte, little-endian, MSB is continuation flag.
/// Values are treated as i32 (like the vanilla Java implementation).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct VarInt(pub i32);

impl From<i32> for VarInt {
    fn from(v: i32) -> Self {
        VarInt(v)
    }
}

impl From<VarInt> for i32 {
    fn from(v: VarInt) -> Self {
        v.0
    }
}

impl VarInt {
    pub fn len(self) -> usize {
        varint_len(self.0)
    }

    pub fn is_empty(self) -> bool {
        self.0 == 0
    }
}

pub fn varint_len(value: i32) -> usize {
    let mut v = value as u32;
    let mut n = 1;
    while v & !0x7F != 0 {
        v >>= 7;
        n += 1;
    }
    n
}

pub fn write_varint(out: &mut Vec<u8>, value: i32) {
    let mut v = value as u32;
    loop {
        if v & !0x7F == 0 {
            out.push(v as u8);
            return;
        }
        out.push(((v & 0x7F) | 0x80) as u8);
        v >>= 7;
    }
}

pub fn read_varint(bytes: &[u8]) -> Result<(VarInt, usize), ProtocolError> {
    let mut value: u32 = 0;
    let mut position = 0u32;
    let mut index = 0usize;
    loop {
        let byte = *bytes.get(index).ok_or(ProtocolError::UnexpectedEof {
            needed: index + 1,
            had: bytes.len(),
        })?;
        index += 1;
        value |= ((byte & 0x7F) as u32) << position;
        if byte & 0x80 == 0 {
            return Ok((VarInt(value as i32), index));
        }
        position += 7;
        if position >= 32 {
            return Err(ProtocolError::VarIntTooBig);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip() {
        for v in [0, 1, 127, 128, 255, 2147483647, -1, -2147483648] {
            let mut buf = Vec::new();
            write_varint(&mut buf, v);
            assert_eq!(buf.len(), varint_len(v));
            let (got, n) = read_varint(&buf).unwrap();
            assert_eq!(got.0, v);
            assert_eq!(n, buf.len());
        }
    }

    #[test]
    fn known_encodings() {
        let mut buf = Vec::new();
        write_varint(&mut buf, 300);
        assert_eq!(buf, vec![0xAC, 0x02]);
        write_varint(&mut buf, 255);
        assert_eq!(&buf[2..], &[0xFF, 0x01]);
    }
}
