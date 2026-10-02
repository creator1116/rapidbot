//! Packet framing, compression and encryption.
//!
//! Mirrors vanilla's Netty pipeline, outermost first:
//! `CipherEncoder` → `Varint21LengthFieldPrepender` → `CompressionEncoder`,
//! and the reverse on the way in.

use std::io::{Read, Write};

use flate2::Compression;
use flate2::read::ZlibDecoder;
use flate2::write::ZlibEncoder;
use rapidbot_buf::{Decode, VarInt, var_int_len};

use crate::cipher::{Decryptor, Encryptor};

/// Frame lengths are a VarInt of at most 3 bytes.
pub const MAX_FRAME_LEN: usize = (1 << 21) - 1;
/// `CompressionDecoder.MAXIMUM_UNCOMPRESSED_LENGTH`.
pub const MAX_UNCOMPRESSED_LEN: usize = 8_388_608;

#[derive(Debug, thiserror::Error)]
pub enum FrameError {
    #[error("frame length wider than 21 bits")]
    LengthTooWide,
    #[error("frame length cannot be zero")]
    ZeroLength,
    #[error("packet too large: {0} bytes")]
    TooLarge(usize),
    #[error("badly compressed packet: {0}")]
    BadCompression(String),
    #[error("malformed frame: {0}")]
    Decode(#[from] rapidbot_buf::DecodeError),
}

/// Builds outgoing frames. Holds compression and encryption state, both of
/// which the server switches on partway through login.
#[derive(Default)]
pub struct FrameEncoder {
    threshold: Option<usize>,
    cipher: Option<Encryptor>,
}

impl FrameEncoder {
    /// A negative threshold disables compression, as in `SetCompression`.
    pub fn set_compression(&mut self, threshold: i32) {
        self.threshold = usize::try_from(threshold).ok();
    }

    pub fn enable_encryption(&mut self, secret: &[u8; 16]) {
        self.cipher = Some(Encryptor::new(secret));
    }

    /// Appends one framed packet to `out`. `packet` is the packet ID followed
    /// by its body.
    pub fn encode(&mut self, packet: &[u8], out: &mut Vec<u8>) -> Result<(), FrameError> {
        let start = out.len();

        match self.threshold {
            None => write_frame(out, packet)?,
            Some(threshold) if packet.len() < threshold => {
                // VarInt(0) marks an uncompressed packet.
                let mut body = Vec::with_capacity(packet.len() + 1);
                body.push(0);
                body.extend_from_slice(packet);
                write_frame(out, &body)?;
            }
            Some(_) => {
                if packet.len() > MAX_UNCOMPRESSED_LEN {
                    return Err(FrameError::TooLarge(packet.len()));
                }
                let mut body = Vec::with_capacity(packet.len() / 2 + 8);
                rapidbot_buf::Encode::encode(&VarInt(packet.len() as i32), &mut body);
                // java.util.zip.Deflater() defaults: zlib wrapper, level 6.
                let mut z = ZlibEncoder::new(body, Compression::new(6));
                z.write_all(packet).expect("writing to a Vec cannot fail");
                let body = z.finish().expect("writing to a Vec cannot fail");
                write_frame(out, &body)?;
            }
        }

        if let Some(cipher) = &mut self.cipher {
            cipher.encrypt(&mut out[start..]);
        }
        Ok(())
    }
}

fn write_frame(out: &mut Vec<u8>, body: &[u8]) -> Result<(), FrameError> {
    if body.len() > MAX_FRAME_LEN {
        return Err(FrameError::TooLarge(body.len()));
    }
    out.reserve(var_int_len(body.len() as i32) + body.len());
    rapidbot_buf::Encode::encode(&VarInt(body.len() as i32), out);
    out.extend_from_slice(body);
    Ok(())
}

/// Splits the incoming byte stream into packets.
#[derive(Default)]
pub struct FrameDecoder {
    threshold: Option<usize>,
    cipher: Option<Decryptor>,
    /// Decrypted bytes not yet consumed as frames.
    buf: Vec<u8>,
    pos: usize,
}

impl FrameDecoder {
    pub fn set_compression(&mut self, threshold: i32) {
        self.threshold = usize::try_from(threshold).ok();
    }

    /// Must be called exactly between the frame carrying the server's
    /// encryption trigger and the next bytes read from the socket. Any bytes
    /// already buffered past that point are decrypted in place.
    pub fn enable_encryption(&mut self, secret: &[u8; 16]) {
        let mut cipher = Decryptor::new(secret);
        cipher.decrypt(&mut self.buf[self.pos..]);
        self.cipher = Some(cipher);
    }

    /// Feeds raw bytes from the socket.
    pub fn feed(&mut self, data: &[u8]) {
        if self.pos > 0 && self.pos == self.buf.len() {
            self.buf.clear();
            self.pos = 0;
        } else if self.pos > 64 * 1024 {
            self.buf.drain(..self.pos);
            self.pos = 0;
        }
        let start = self.buf.len();
        self.buf.extend_from_slice(data);
        if let Some(cipher) = &mut self.cipher {
            cipher.decrypt(&mut self.buf[start..]);
        }
    }

    /// Returns the next complete packet (ID + body), decompressed, if one
    /// has fully arrived.
    pub fn next_packet(&mut self) -> Result<Option<Vec<u8>>, FrameError> {
        let mut rest = &self.buf[self.pos..];
        let before = rest.len();

        // Varint21FrameDecoder: at most 3 length bytes.
        let mut len = 0usize;
        let mut complete = false;
        for i in 0..3 {
            let Some(&byte) = rest.get(i) else { return Ok(None) };
            len |= ((byte & 0x7f) as usize) << (7 * i);
            if byte & 0x80 == 0 {
                rest = &rest[i + 1..];
                complete = true;
                break;
            }
        }
        if !complete {
            return Err(FrameError::LengthTooWide);
        }
        if len == 0 {
            return Err(FrameError::ZeroLength);
        }
        if rest.len() < len {
            return Ok(None);
        }

        let frame = &rest[..len];
        self.pos += before - rest.len() + len;
        self.decompress(frame).map(Some)
    }

    fn decompress(&self, frame: &[u8]) -> Result<Vec<u8>, FrameError> {
        if self.threshold.is_none() {
            return Ok(frame.to_vec());
        }
        let mut frame = frame;
        let uncompressed_len = VarInt::decode(&mut frame)?.0;
        if uncompressed_len == 0 {
            return Ok(frame.to_vec());
        }
        let uncompressed_len = usize::try_from(uncompressed_len)
            .map_err(|_| FrameError::BadCompression(format!("negative length {uncompressed_len}")))?;
        if uncompressed_len > MAX_UNCOMPRESSED_LEN {
            return Err(FrameError::TooLarge(uncompressed_len));
        }
        let mut out = Vec::with_capacity(uncompressed_len);
        ZlibDecoder::new(frame)
            .take(uncompressed_len as u64 + 1)
            .read_to_end(&mut out)
            .map_err(|e| FrameError::BadCompression(e.to_string()))?;
        if out.len() != uncompressed_len {
            return Err(FrameError::BadCompression(format!(
                "declared {uncompressed_len} bytes, got {}",
                out.len()
            )));
        }
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn roundtrip(enc: &mut FrameEncoder, dec: &mut FrameDecoder, packet: &[u8]) -> Vec<u8> {
        let mut wire = Vec::new();
        enc.encode(packet, &mut wire).unwrap();
        // Feed one byte at a time to exercise partial frames.
        let mut got = None;
        for b in &wire {
            assert!(got.is_none(), "frame completed early");
            dec.feed(std::slice::from_ref(b));
            got = dec.next_packet().unwrap();
        }
        assert_eq!(got.as_deref(), Some(packet));
        wire
    }

    #[test]
    fn plain() {
        let (mut e, mut d) = (FrameEncoder::default(), FrameDecoder::default());
        assert_eq!(roundtrip(&mut e, &mut d, &[0x00, 0x01, 0x02]), [3, 0, 1, 2]);
    }

    #[test]
    fn compression_below_and_above_threshold() {
        let (mut e, mut d) = (FrameEncoder::default(), FrameDecoder::default());
        e.set_compression(256);
        d.set_compression(256);
        assert_eq!(roundtrip(&mut e, &mut d, &[7, 8]), [3, 0, 7, 8]);

        let big = vec![42u8; 1000];
        let wire = roundtrip(&mut e, &mut d, &big);
        assert!(wire.len() < 100);
        // Data length after the frame length: VarInt(1000) = e8 07.
        assert_eq!(&wire[1..3], &[0xe8, 0x07]);
    }

    #[test]
    fn encrypted_and_compressed() {
        let (mut e, mut d) = (FrameEncoder::default(), FrameDecoder::default());
        let key = *b"0123456789abcdef";
        e.set_compression(64);
        d.set_compression(64);
        e.enable_encryption(&key);
        d.enable_encryption(&key);
        for n in [1, 63, 64, 500, 5000] {
            let packet: Vec<u8> = (0..n).map(|i| (i * 7) as u8).collect();
            roundtrip(&mut e, &mut d, &packet);
        }
    }

    #[test]
    fn encryption_enabled_with_buffered_bytes() {
        let key = *b"fedcba9876543210";
        let mut plain = FrameEncoder::default();
        let mut crypt = FrameEncoder::default();
        crypt.enable_encryption(&key);

        let mut wire = Vec::new();
        plain.encode(&[1, 2, 3], &mut wire).unwrap();
        crypt.encode(&[4, 5, 6], &mut wire).unwrap();

        // Both frames arrive in one read, before we know to decrypt.
        let mut d = FrameDecoder::default();
        d.feed(&wire);
        assert_eq!(d.next_packet().unwrap().unwrap(), [1, 2, 3]);
        d.enable_encryption(&key);
        assert_eq!(d.next_packet().unwrap().unwrap(), [4, 5, 6]);
    }

    #[test]
    fn rejects_bad_frames() {
        let mut d = FrameDecoder::default();
        d.feed(&[0x00]);
        assert!(matches!(d.next_packet(), Err(FrameError::ZeroLength)));

        let mut d = FrameDecoder::default();
        d.feed(&[0x80, 0x80, 0x80, 0x01]);
        assert!(matches!(d.next_packet(), Err(FrameError::LengthTooWide)));
    }
}
