use std::io::Write;

use loadstone_protocol::ProtocolError;
use loadstone_protocol::{read_varint, write_varint};
use thiserror::Error;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};

/// Hard cap on a single uncompressed packet frame (vanilla default).
pub const MAX_PACKET_SIZE: usize = 2 * 1024 * 1024;

#[derive(Debug, Error)]
pub enum FrameError {
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
    #[error("protocol error: {0}")]
    Protocol(#[from] ProtocolError),
    #[error("packet length {0} exceeds maximum {MAX_PACKET_SIZE}")]
    TooLarge(usize),
    #[error("compressed packet claims inflated size {0} exceeds maximum")]
    InflatedTooLarge(usize),
    #[error("compressed packet smaller than declared data length")]
    MalformedCompressed,
}

/// A decoded inbound frame with compression already handled.
pub struct InboundFrame {
    pub packet_id: i32,
    pub payload: Vec<u8>,
}

pub async fn read_frame<R>(
    reader: &mut R,
    compression_threshold: Option<i32>,
) -> Result<InboundFrame, FrameError>
where
    R: AsyncRead + Unpin,
{
    let len = read_varint_async(reader).await?;
    let len = if len < 0 {
        return Err(FrameError::TooLarge(len as usize));
    } else {
        len as usize
    };
    if len > MAX_PACKET_SIZE {
        return Err(FrameError::TooLarge(len));
    }

    let mut body = vec![0u8; len];
    reader.read_exact(&mut body).await?;

    let mut body = match compression_threshold {
        None => body,
        Some(_) => {
            let (data_len, n) = read_varint(&body)?;
            let rest = &body[n..];
            if data_len.0 == 0 {
                rest.to_vec()
            } else {
                let data_len = data_len.0 as usize;
                if data_len > MAX_PACKET_SIZE {
                    return Err(FrameError::InflatedTooLarge(data_len));
                }
                let mut out = Vec::with_capacity(data_len);
                let mut decoder = flate2::write::ZlibDecoder::new(&mut out);
                decoder.write_all(rest)?;
                decoder.finish()?;
                if out.len() != data_len {
                    return Err(FrameError::MalformedCompressed);
                }
                out
            }
        }
    };

    let (packet_id, n) = read_varint(&body)?;
    body.drain(..n);
    Ok(InboundFrame {
        packet_id: packet_id.0,
        payload: body,
    })
}

pub async fn write_frame<W>(
    writer: &mut W,
    packet_id: i32,
    payload: &[u8],
    compression_threshold: Option<i32>,
) -> Result<(), FrameError>
where
    W: AsyncWrite + Unpin,
{
    let mut data = Vec::with_capacity(payload.len() + 5);
    write_varint(&mut data, packet_id);
    data.extend_from_slice(payload);

    let mut frame = Vec::with_capacity(data.len() + 5);
    match compression_threshold {
        None => {
            write_varint(&mut frame, data.len() as i32);
            frame.extend_from_slice(&data);
        }
        Some(threshold) => {
            if data.len() >= threshold as usize {
                let mut compressed = Vec::new();
                let mut enc = flate2::write::ZlibEncoder::new(
                    &mut compressed,
                    flate2::Compression::default(),
                );
                enc.write_all(&data)?;
                enc.finish()?;

                let mut inner = Vec::with_capacity(compressed.len() + 5);
                write_varint(&mut inner, data.len() as i32);
                inner.extend_from_slice(&compressed);
                write_varint(&mut frame, inner.len() as i32);
                frame.extend_from_slice(&inner);
            } else {
                let mut inner = Vec::with_capacity(data.len() + 5);
                write_varint(&mut inner, 0);
                inner.extend_from_slice(&data);
                write_varint(&mut frame, inner.len() as i32);
                frame.extend_from_slice(&inner);
            }
        }
    }

    writer.write_all(&frame).await?;
    writer.flush().await?;
    Ok(())
}

async fn read_varint_async<R>(reader: &mut R) -> Result<i32, FrameError>
where
    R: AsyncRead + Unpin,
{
    let mut value: u32 = 0;
    let mut position = 0u32;
    loop {
        let byte = reader.read_u8().await?;
        value |= ((byte & 0x7F) as u32) << position;
        if byte & 0x80 == 0 {
            return Ok(value as i32);
        }
        position += 7;
        if position >= 32 {
            return Err(FrameError::Protocol(ProtocolError::VarIntTooBig));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::duplex;

    #[tokio::test]
    async fn frame_roundtrip_uncompressed() {
        roundtrip(None, &[0x01, 0x02, 0x03]).await;
    }

    #[tokio::test]
    async fn frame_roundtrip_compressed() {
        let big = vec![0xAB; 512];
        roundtrip(Some(64), &big).await;
    }

    #[tokio::test]
    async fn frame_roundtrip_small_compressed() {
        // Below threshold: data length prefix 0, body sent raw.
        roundtrip(Some(64), &[0x42]).await;
    }

    async fn roundtrip(threshold: Option<i32>, payload: &[u8]) {
        let (mut a, mut b) = duplex(1024 * 1024);
        let p = payload.to_vec();
        let t = threshold;
        let writer = tokio::spawn(async move {
            write_frame(&mut a, 0x2A, &p, t).await.unwrap();
        });
        let frame = read_frame(&mut b, threshold).await.unwrap();
        writer.await.unwrap();
        assert_eq!(frame.packet_id, 0x2A);
        assert_eq!(frame.payload, payload);
    }
}
