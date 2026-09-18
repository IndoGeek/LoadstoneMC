//! AES-128/CFB8 cipher, matching Java's `AES/CFB8/NoPadding`, and an async
//! stream that transparently encrypts/decrypts a `TcpStream`.
//!
//! CFB8: each output byte is `plaintext_byte ^ first_byte_of_E_k(register)`,
//! where the register is the last 16 ciphertext bytes (starting from the IV).
//! Because the IV is the shared secret itself, encryption and decryption are
//! both keyed by the same 16 bytes.

use aes::cipher::generic_array::{typenum::U16, GenericArray};
use aes::cipher::{BlockEncrypt, KeyInit};
use aes::Aes128;
use std::io;
use std::pin::Pin;
use std::task::{Context, Poll};
use tokio::io::{AsyncRead, AsyncWrite, ReadBuf};
use tokio::net::TcpStream;

const BLOCK_LEN: usize = 16;
const KEY_LEN: usize = 16;

pub struct Cfb8 {
    cipher: Aes128,
    /// Last 16 bytes of ciphertext (the feedback register).
    feedback: [u8; BLOCK_LEN],
    encrypting: bool,
}

impl Cfb8 {
    pub fn new(key: &[u8; KEY_LEN], iv: &[u8; KEY_LEN], encrypting: bool) -> Self {
        Self {
            cipher: Aes128::new(key.into()),
            feedback: *iv,
            encrypting,
        }
    }

    /// CFB8 takes the first byte of `E_k(feedback)` for each byte of data, and the
    /// register has already shifted by then, so the block encryption is redone
    /// every byte. Reusing one block's worth of keystream (a 16-byte cache) is
    /// indistinguishable in a round trip against this module but produces bytes
    /// no vanilla client can read — see the pinned vectors in the tests below.
    fn next_keystream_byte(&mut self) -> u8 {
        let mut block: GenericArray<u8, U16> = GenericArray::default();
        block.copy_from_slice(&self.feedback);
        self.cipher.encrypt_block(&mut block);
        block[0]
    }

    /// Transform `data` in place. Encryption feeds the output byte back into
    /// the register; decryption feeds the input (ciphertext) byte back.
    pub fn transform(&mut self, data: &mut [u8]) {
        for byte in data.iter_mut() {
            let keystream_byte = self.next_keystream_byte();
            let input = *byte;
            let output = input ^ keystream_byte;
            let feedback = if self.encrypting { output } else { input };
            self.feedback.copy_within(1..BLOCK_LEN, 0);
            self.feedback[BLOCK_LEN - 1] = feedback;
            *byte = output;
        }
    }
}

/// A `TcpStream` that decrypts inbound reads and encrypts outbound writes.
pub struct EncryptedStream {
    inner: TcpStream,
    decrypt: Cfb8,
    encrypt: Cfb8,
    /// Decrypted bytes not yet handed to the caller.
    read_leftover: Vec<u8>,
    read_pos: usize,
    /// Encrypted bytes not yet written to `inner`.
    write_pending: Vec<u8>,
}

impl EncryptedStream {
    pub fn new(stream: TcpStream, key_and_iv: &[u8; KEY_LEN]) -> Self {
        Self {
            inner: stream,
            decrypt: Cfb8::new(key_and_iv, key_and_iv, false),
            encrypt: Cfb8::new(key_and_iv, key_and_iv, true),
            read_leftover: Vec::new(),
            read_pos: 0,
            write_pending: Vec::new(),
        }
    }

    fn flush_pending(&mut self, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        while !self.write_pending.is_empty() {
            match Pin::new(&mut self.inner).poll_write(cx, &self.write_pending) {
                Poll::Pending => return Poll::Pending,
                Poll::Ready(Err(e)) => return Poll::Ready(Err(e)),
                Poll::Ready(Ok(0)) => return Poll::Ready(Ok(())),
                Poll::Ready(Ok(n)) => {
                    self.write_pending.drain(..n);
                }
            }
        }
        Poll::Ready(Ok(()))
    }
}

impl AsyncRead for EncryptedStream {
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        let this = &mut *self;

        if this.read_pos < this.read_leftover.len() {
            let n = (this.read_leftover.len() - this.read_pos).min(buf.remaining());
            buf.put_slice(&this.read_leftover[this.read_pos..this.read_pos + n]);
            this.read_pos += n;
            if this.read_pos == this.read_leftover.len() {
                this.read_leftover.clear();
                this.read_pos = 0;
            }
            return Poll::Ready(Ok(()));
        }

        let cap = buf.remaining();
        if cap == 0 {
            return Poll::Ready(Ok(()));
        }

        let mut raw = vec![0u8; cap];
        let mut raw_buf = ReadBuf::new(&mut raw);
        match Pin::new(&mut this.inner).poll_read(cx, &mut raw_buf) {
            Poll::Pending => Poll::Pending,
            Poll::Ready(Err(e)) => Poll::Ready(Err(e)),
            Poll::Ready(Ok(())) => {
                let filled = raw_buf.filled();
                if filled.is_empty() {
                    return Poll::Ready(Ok(())); // EOF
                }
                let filled_len = filled.len();
                let slice = &mut raw[..filled_len];
                this.decrypt.transform(slice);
                buf.put_slice(slice);
                Poll::Ready(Ok(()))
            }
        }
    }
}

impl AsyncWrite for EncryptedStream {
    fn poll_write(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<io::Result<usize>> {
        let this = self.get_mut();
        let mut encrypted = vec![0u8; buf.len()];
        encrypted.copy_from_slice(buf);
        this.encrypt.transform(&mut encrypted);
        this.write_pending.extend_from_slice(&encrypted);

        match this.flush_pending(cx) {
            Poll::Pending => {}
            Poll::Ready(Ok(())) => {}
            Poll::Ready(Err(e)) => return Poll::Ready(Err(e)),
        }
        // Accept the whole slice even if only part flushed: the remainder is
        // buffered in `write_pending` and drained by `poll_flush`.
        Poll::Ready(Ok(buf.len()))
    }

    fn poll_flush(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        let this = self.get_mut();
        match this.flush_pending(cx) {
            Poll::Pending => return Poll::Pending,
            Poll::Ready(Err(e)) => return Poll::Ready(Err(e)),
            Poll::Ready(Ok(())) => {}
        }
        Pin::new(&mut this.inner).poll_flush(cx)
    }

    fn poll_shutdown(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        let this = self.get_mut();
        match this.flush_pending(cx) {
            Poll::Pending => return Poll::Pending,
            Poll::Ready(Err(e)) => return Poll::Ready(Err(e)),
            Poll::Ready(Ok(())) => {}
        }
        Pin::new(&mut this.inner).poll_shutdown(cx)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hex(bytes: &[u8]) -> String {
        bytes.iter().map(|b| format!("{b:02x}")).collect()
    }

    /// Pinned against two independent implementations — `openssl enc -aes-128-cfb8`
    /// and `cryptography`'s AES/CFB8, which agree byte for byte. A round trip
    /// through this module can never catch a keystream bug because both ends
    /// share the implementation, so these vectors are the only guard that a real
    /// vanilla client would still be able to read the stream.
    #[test]
    fn matches_reference_vectors() {
        let key: [u8; 16] = [
            0x01, 0x02, 0x03, 0x04, 0x05, 0x06, 0x07, 0x08, 0x09, 0x0a, 0x0b, 0x0c, 0x0d, 0x0e,
            0x0f, 0x10,
        ];
        let plain = b"loadstone cfb8 known answer".to_vec();

        let mut buffer = plain.clone();
        Cfb8::new(&key, &key, true).transform(&mut buffer);
        assert_eq!(
            hex(&buffer),
            "58a6a5c55c0300b828fccea0fce6b380bea2da82e7caf537f6a07b"
        );

        let mut back = buffer.clone();
        Cfb8::new(&key, &key, false).transform(&mut back);
        assert_eq!(back, plain);

        // A full block boundary: the keystream must be re-derived from the shifted
        // register every byte, not once per 16 bytes.
        let key2 = [0x2bu8; 16];
        let iv2 = [0x7eu8; 16];
        let plain2: Vec<u8> = (0..64u8).collect();
        let mut buffer2 = plain2.clone();
        Cfb8::new(&key2, &iv2, true).transform(&mut buffer2);
        assert_eq!(
            hex(&buffer2),
            "2bd8d9aa051cbb04bf2a6081bb77408339a7f49d51516201167a113bf96eafc4\
             28a449a6cc0ebb61db3b6ed63b1ecb12bb728d9225e9d9e77d07db1debfcac4c"
        );
    }

    /// Frames are encrypted in pieces; a state reset between calls would corrupt
    /// everything after the first frame.
    #[test]
    fn chunked_transform_matches_one_shot() {
        let key = [0x11u8; 16];
        let data: Vec<u8> = (0..97u8).collect();

        let mut one_shot = data.clone();
        Cfb8::new(&key, &key, true).transform(&mut one_shot);

        let mut chunked = data.clone();
        let mut cipher = Cfb8::new(&key, &key, true);
        for chunk in chunked.chunks_mut(7) {
            cipher.transform(chunk);
        }

        assert_eq!(one_shot, chunked);
    }

    #[test]
    fn cfb8_roundtrip() {
        let key = [0x1Fu8; 16];
        let data = "hello loadstone, this is a longer message to force blocks".as_bytes();
        let mut encrypted = data.to_vec();
        Cfb8::new(&key, &key, true).transform(&mut encrypted);
        let mut decrypted = encrypted.clone();
        Cfb8::new(&key, &key, false).transform(&mut decrypted);
        assert_eq!(&decrypted, data);
    }

    #[test]
    fn decrypt_feedback_uses_input_byte() {
        // Encryption: c_i = p_i ^ ks_i, feedback receives c_i.
        // Decryption: p_i = c_i ^ ks_i, feedback must receive c_i, not p_i.
        // If a decryptor fed plaintext back, encrypt-then-decrypt would fail
        // on blocks after the first keystream refresh; force that here.
        let key = [0x77u8; 16];
        let mut enc = Cfb8::new(&key, &key, true);
        let mut plain = vec![0u8; 48];
        plain[10] = 0x55;
        plain[30] = 0xAA;
        plain[31] = 0x11;
        let mut cipher = plain.clone();
        enc.transform(&mut cipher);

        let mut dec = Cfb8::new(&key, &key, false);
        dec.transform(&mut cipher);
        assert_eq!(plain, cipher);
    }
}
