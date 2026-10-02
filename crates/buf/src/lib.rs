//! Minecraft wire types.
//!
//! Encoding must be byte-exact with vanilla's `FriendlyByteBuf`: the server
//! disconnects on trailing bytes, and any deviation is a fingerprint.

// Lets the derive macros' `::rapidbot_buf` paths resolve inside this crate.
extern crate self as rapidbot_buf;

mod primitives;
mod var;

pub use rapidbot_buf_derive::{Decode, Encode};
pub use var::{DecodeVar, EncodeVar, VarInt, VarLong, read_var_int, var_int_len, write_var_int};

/// Vanilla's default maximum string length, in UTF-16 code units.
pub const MAX_STRING_LEN: usize = 32767;

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum DecodeError {
    #[error("unexpected end of buffer")]
    UnexpectedEof,
    #[error("VarInt too big")]
    VarIntTooBig,
    #[error("VarLong too big")]
    VarLongTooBig,
    #[error("string is {len} UTF-16 units, max is {max}")]
    StringTooLong { len: usize, max: usize },
    #[error("string is not valid UTF-8")]
    InvalidUtf8,
    #[error("negative length {0}")]
    NegativeLength(i32),
    #[error("invalid {ty} discriminant {value}")]
    InvalidDiscriminant { ty: &'static str, value: i32 },
    #[error("{0}")]
    Custom(String),
}

pub trait Encode {
    fn encode(&self, buf: &mut Vec<u8>);
}

pub trait Decode: Sized {
    /// Reads from the front of `buf`, advancing it past the consumed bytes.
    fn decode(buf: &mut &[u8]) -> Result<Self, DecodeError>;
}

/// Takes `n` bytes off the front of `buf`.
pub fn take<'a>(buf: &mut &'a [u8], n: usize) -> Result<&'a [u8], DecodeError> {
    if buf.len() < n {
        return Err(DecodeError::UnexpectedEof);
    }
    let (head, tail) = buf.split_at(n);
    *buf = tail;
    Ok(head)
}

/// Encodes a value into a fresh buffer.
pub fn to_bytes<T: Encode + ?Sized>(value: &T) -> Vec<u8> {
    let mut buf = Vec::new();
    value.encode(&mut buf);
    buf
}

/// Every remaining byte of the packet, with no length prefix.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RemainingBytes(pub Vec<u8>);

impl Encode for RemainingBytes {
    fn encode(&self, buf: &mut Vec<u8>) {
        buf.extend_from_slice(&self.0);
    }
}

impl Decode for RemainingBytes {
    fn decode(buf: &mut &[u8]) -> Result<Self, DecodeError> {
        let bytes = buf.to_vec();
        *buf = &[];
        Ok(Self(bytes))
    }
}

/// `writeByteArray`: VarInt length then raw bytes.
#[derive(Debug, Clone, Default, PartialEq, Eq, Hash)]
pub struct ByteArray(pub Vec<u8>);

impl Encode for ByteArray {
    fn encode(&self, buf: &mut Vec<u8>) {
        var::write_var_int(buf, self.0.len() as i32);
        buf.extend_from_slice(&self.0);
    }
}

impl Decode for ByteArray {
    fn decode(buf: &mut &[u8]) -> Result<Self, DecodeError> {
        let len = primitives::read_len(buf)?;
        Ok(Self(take(buf, len)?.to_vec()))
    }
}

/// A namespaced identifier such as `minecraft:brand` (`Identifier`, formerly
/// `ResourceLocation`). Written as a plain string.
#[derive(Debug, Clone, Default, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Identifier(pub String);

impl Identifier {
    pub fn new(s: impl Into<String>) -> Self {
        Self(s.into())
    }

    /// `minecraft:` prefixed ids compare equal to their bare form in vanilla;
    /// this returns the fully qualified form.
    pub fn normalized(&self) -> std::borrow::Cow<'_, str> {
        if self.0.contains(':') {
            self.0.as_str().into()
        } else {
            format!("minecraft:{}", self.0).into()
        }
    }
}

impl std::fmt::Display for Identifier {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl From<&str> for Identifier {
    fn from(s: &str) -> Self {
        Self(s.to_owned())
    }
}

impl Encode for Identifier {
    fn encode(&self, buf: &mut Vec<u8>) {
        self.0.encode(buf);
    }
}

impl Decode for Identifier {
    fn decode(buf: &mut &[u8]) -> Result<Self, DecodeError> {
        String::decode(buf).map(Self)
    }
}

/// A string with a smaller maximum length than [`MAX_STRING_LEN`].
///
/// Vanilla enforces per-field limits (16 for usernames, 256 for chat, ...)
/// on both read and write.
#[derive(Debug, Clone, Default, PartialEq, Eq, Hash)]
pub struct BoundedString<const MAX: usize>(pub String);

impl<const MAX: usize> Encode for BoundedString<MAX> {
    fn encode(&self, buf: &mut Vec<u8>) {
        debug_assert!(self.0.encode_utf16().count() <= MAX, "string exceeds {MAX}");
        self.0.encode(buf);
    }
}

impl<const MAX: usize> Decode for BoundedString<MAX> {
    fn decode(buf: &mut &[u8]) -> Result<Self, DecodeError> {
        primitives::decode_string(buf, MAX).map(Self)
    }
}

impl<const MAX: usize> From<&str> for BoundedString<MAX> {
    fn from(s: &str) -> Self {
        Self(s.to_owned())
    }
}

#[cfg(test)]
mod tests;
