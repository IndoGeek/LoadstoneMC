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

    pub fn read_bytes(&mut self, len: usize) -> Result<&'a [u8], ProtocolError> {
        self.take(len)
    }

    pub fn read_rest(&mut self) -> &'a [u8] {
        let rest = &self.data[self.pos..];
        self.pos = self.data.len();
        rest
    }
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
