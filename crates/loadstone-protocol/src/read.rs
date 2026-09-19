use crate::error::ProtocolError;
use crate::varint::read_varint;

/// Zero-copy reader over a packet payload (packet id already stripped).
pub struct PacketReader<'a> {
    data: &'a [u8],
    pos: usize,
}

impl<'a> PacketReader<'a> {
    pub fn new(data: &'a [u8]) -> Self {
        Self { data, pos: 0 }
    }

    pub fn remaining(&self) -> usize {
        self.data.len().saturating_sub(self.pos)
    }

    pub fn is_empty(&self) -> bool {
        self.remaining() == 0
    }

    fn take(&mut self, len: usize) -> Result<&'a [u8], ProtocolError> {
        if self.remaining() < len {
            return Err(ProtocolError::UnexpectedEof {
                needed: self.pos + len,
                had: self.data.len(),
            });
        }
        let slice = &self.data[self.pos..self.pos + len];
        self.pos += len;
        Ok(slice)
    }

    pub fn read_u8(&mut self) -> Result<u8, ProtocolError> {
        Ok(self.take(1)?[0])
    }

    pub fn read_i8(&mut self) -> Result<i8, ProtocolError> {
        Ok(self.read_u8()? as i8)
    }

    pub fn read_bool(&mut self) -> Result<bool, ProtocolError> {
        match self.read_u8()? {
            0 => Ok(false),
            1 => Ok(true),
            other => Err(ProtocolError::InvalidBool(other)),
        }
    }

    pub fn read_i16(&mut self) -> Result<i16, ProtocolError> {
        let b = self.take(2)?;
        Ok(i16::from_be_bytes([b[0], b[1]]))
    }

    pub fn read_u16(&mut self) -> Result<u16, ProtocolError> {
        let b = self.take(2)?;
        Ok(u16::from_be_bytes([b[0], b[1]]))
    }

    pub fn read_i32(&mut self) -> Result<i32, ProtocolError> {
        let b = self.take(4)?;
        Ok(i32::from_be_bytes([b[0], b[1], b[2], b[3]]))
    }

    pub fn read_i64(&mut self) -> Result<i64, ProtocolError> {
        let b = self.take(8)?;
        Ok(i64::from_be_bytes([
            b[0], b[1], b[2], b[3], b[4], b[5], b[6], b[7],
        ]))
    }

    pub fn read_u64(&mut self) -> Result<u64, ProtocolError> {
        Ok(self.read_i64()? as u64)
    }

    pub fn read_f32(&mut self) -> Result<f32, ProtocolError> {
        Ok(f32::from_bits(self.read_i32()? as u32))
    }

    pub fn read_f64(&mut self) -> Result<f64, ProtocolError> {
        Ok(f64::from_bits(self.read_i64()? as u64))
    }

    pub fn read_varint(&mut self) -> Result<i32, ProtocolError> {
        let (v, n) = read_varint(&self.data[self.pos..])?;
        self.pos += n;
        Ok(v.0)
    }

    pub fn read_uuid(&mut self) -> Result<uuid::Uuid, ProtocolError> {
        let b = self.take(16)?;
        let mut arr = [0u8; 16];
        arr.copy_from_slice(b);
        Ok(uuid::Uuid::from_bytes(arr))
    }

    pub fn read_string(&mut self) -> Result<&'a str, ProtocolError> {
        let len = self.read_varint()?;
        if len < 0 {
            return Err(ProtocolError::InvalidStringLength(len));
        }
        let bytes = self.take(len as usize)?;
        std::str::from_utf8(bytes).map_err(|_| ProtocolError::InvalidUtf8)
    }

    /// Reads an optional "anonymous NBT" value from the wire: a single
    /// `TAG_End` byte means absent, otherwise this consumes a type-byte framed
    /// tag (root name omitted) and returns its raw bytes.
    pub fn read_anonymous_nbt(&mut self) -> Result<Option<&'a [u8]>, ProtocolError> {
        match anonymous_tag_end(&self.data[self.pos..])? {
            None => {
                self.pos += 1;
                Ok(None)
            }
            Some(end) => self.take(end).map(Some),
        }
    }

    pub fn read_bytes(&mut self, len: usize) -> Result<&'a [u8], ProtocolError> {
        self.take(len)
    }

    pub fn read_rest(&mut self) -> &'a [u8] {
        let rest = &self.data[self.pos..];
        self.pos = self.data.len();
        rest
    }
}

/// Returns the index just past a single anonymous-NBT tag starting at `data[0]`
/// (type byte + payload, no root name, no length prefix), or `None` when the
/// tag is the `TAG_End` absent-value sentinel.
fn anonymous_tag_end(data: &[u8]) -> Result<Option<usize>, ProtocolError> {
    let ty = match data.first() {
        Some(&0) => return Ok(None),
        Some(&t) => t,
        None => return Err(ProtocolError::UnexpectedEof { needed: 1, had: 0 }),
    };
    let mut i = 1;
    match ty {
        0x01 => i += 1,
        0x02 | 0x03 | 0x05 | 0x0B => i += 4,
        0x04 | 0x06 | 0x0C => i += 8,
        0x07 => {
            let n = signed_ok(data, i, 4)? as usize;
            i += 4 + n;
        }
        0x08 => {
            let n = u16::from_be_bytes([
                *data.get(i).ok_or(short(i + 2, data.len()))?,
                *data.get(i + 1).ok_or(short(i + 2, data.len()))?,
            ]) as usize;
            i += 2 + n;
        }
        0x09 => {
            if data.get(i).is_none() {
                return Err(short(i + 1, data.len()));
            }
            let element_ty = data[i];
            i += 1;
            let n = signed_ok(data, i, 4)? as usize;
            i += 4;
            for _ in 0..n {
                i = value_end(data, element_ty, i)?;
            }
        }
        0x0A => loop {
            let ty = *data.get(i).ok_or(short(i + 1, data.len()))?;
            if ty == 0 {
                i += 1;
                break;
            }
            let n = u16::from_be_bytes([
                *data.get(i + 1).ok_or(short(i + 3, data.len()))?,
                *data.get(i + 2).ok_or(short(i + 3, data.len()))?,
            ]) as usize;
            i += 3 + n;
            i = value_end(data, ty, i)?;
        },
        other => {
            return Err(ProtocolError::InvalidEnum(i32::from(other)));
        }
    }
    if i > data.len() {
        Err(short(i, data.len()))
    } else {
        Ok(Some(i))
    }
}

fn signed_ok(data: &[u8], i: usize, n: usize) -> Result<i32, ProtocolError> {
    if data.len() < i + n {
        return Err(short(i + n, data.len()));
    }
    let mut bytes = [0u8; 4];
    bytes[..n].copy_from_slice(&data[i..i + n]);
    Ok(i32::from_be_bytes(bytes))
}

/// End of a list element or nested value: the element has no name, so skip just
/// its payload.
fn value_end(data: &[u8], ty: u8, start: usize) -> Result<usize, ProtocolError> {
    match ty {
        0x01 => Ok(start + 1),
        0x02 => Ok(start + 2),
        0x03 | 0x05 | 0x0B => Ok(start + 4),
        0x04 | 0x06 | 0x0C => Ok(start + 8),
        0x07 => {
            let n = signed_ok(data, start, 4)? as usize;
            Ok(start + 4 + n)
        }
        0x08 => {
            let n = u16::from_be_bytes([
                *data.get(start).ok_or(short(start + 2, data.len()))?,
                *data.get(start + 1).ok_or(short(start + 2, data.len()))?,
            ]) as usize;
            Ok(start + 2 + n)
        }
        0x09 => {
            let element_ty = *data.get(start).ok_or(short(start + 1, data.len()))?;
            let n = signed_ok(data, start + 1, 4)? as usize;
            let mut i = start + 5;
            for _ in 0..n {
                i = value_end(data, element_ty, i)?;
            }
            Ok(i)
        }
        0x0A => {
            let mut i = start;
            loop {
                let ty = *data.get(i).ok_or(short(i + 1, data.len()))?;
                if ty == 0 {
                    i += 1;
                    break;
                }
                let n = u16::from_be_bytes([
                    *data.get(i + 1).ok_or(short(i + 3, data.len()))?,
                    *data.get(i + 2).ok_or(short(i + 3, data.len()))?,
                ]) as usize;
                i += 3 + n;
                i = value_end(data, ty, i)?;
            }
            Ok(i)
        }
        other => Err(ProtocolError::InvalidEnum(i32::from(other))),
    }
}

fn short(needed: usize, had: usize) -> ProtocolError {
    ProtocolError::UnexpectedEof { needed, had }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_primitives() {
        let data = [0x01, 0x00, 0x2A, 0xFF, 0xFF, 0xFF, 0xFF];
        let mut r = PacketReader::new(&data);
        assert!(r.read_bool().unwrap());
        assert_eq!(r.read_i16().unwrap(), 42);
        assert_eq!(r.read_i32().unwrap(), -1);
        assert!(r.is_empty());
    }
}
