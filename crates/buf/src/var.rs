use crate::{Decode, DecodeError, Encode, take};

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct VarInt(pub i32);

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct VarLong(pub i64);

/// Number of bytes `value` takes as a VarInt.
pub const fn var_int_len(value: i32) -> usize {
    let v = value as u32;
    match v {
        0..=0x7f => 1,
        0x80..=0x3fff => 2,
        0x4000..=0x1f_ffff => 3,
        0x20_0000..=0x0fff_ffff => 4,
        _ => 5,
    }
}

pub fn write_var_int(buf: &mut Vec<u8>, value: i32) {
    let mut v = value as u32;
    loop {
        if v & !0x7f == 0 {
            buf.push(v as u8);
            return;
        }
        buf.push((v as u8 & 0x7f) | 0x80);
        v >>= 7;
    }
}

pub fn read_var_int(buf: &mut &[u8]) -> Result<i32, DecodeError> {
    let mut value = 0u32;
    for i in 0..5 {
        let byte = take(buf, 1)?[0];
        value |= ((byte & 0x7f) as u32) << (7 * i);
        if byte & 0x80 == 0 {
            return Ok(value as i32);
        }
    }
    Err(DecodeError::VarIntTooBig)
}

pub fn write_var_long(buf: &mut Vec<u8>, value: i64) {
    let mut v = value as u64;
    loop {
        if v & !0x7f == 0 {
            buf.push(v as u8);
            return;
        }
        buf.push((v as u8 & 0x7f) | 0x80);
        v >>= 7;
    }
}

pub fn read_var_long(buf: &mut &[u8]) -> Result<i64, DecodeError> {
    let mut value = 0u64;
    for i in 0..10 {
        let byte = take(buf, 1)?[0];
        value |= ((byte & 0x7f) as u64) << (7 * i);
        if byte & 0x80 == 0 {
            return Ok(value as i64);
        }
    }
    Err(DecodeError::VarLongTooBig)
}

impl Encode for VarInt {
    fn encode(&self, buf: &mut Vec<u8>) {
        write_var_int(buf, self.0);
    }
}

impl Decode for VarInt {
    fn decode(buf: &mut &[u8]) -> Result<Self, DecodeError> {
        read_var_int(buf).map(Self)
    }
}

impl Encode for VarLong {
    fn encode(&self, buf: &mut Vec<u8>) {
        write_var_long(buf, self.0);
    }
}

impl Decode for VarLong {
    fn decode(buf: &mut &[u8]) -> Result<Self, DecodeError> {
        read_var_long(buf).map(Self)
    }
}

/// Variable-length encoding for fields marked `#[var]` in derived structs.
pub trait EncodeVar {
    fn encode_var(&self, buf: &mut Vec<u8>);
}

pub trait DecodeVar: Sized {
    fn decode_var(buf: &mut &[u8]) -> Result<Self, DecodeError>;
}

impl EncodeVar for i32 {
    fn encode_var(&self, buf: &mut Vec<u8>) {
        write_var_int(buf, *self);
    }
}

impl DecodeVar for i32 {
    fn decode_var(buf: &mut &[u8]) -> Result<Self, DecodeError> {
        read_var_int(buf)
    }
}

impl EncodeVar for u32 {
    fn encode_var(&self, buf: &mut Vec<u8>) {
        write_var_int(buf, *self as i32);
    }
}

impl DecodeVar for u32 {
    fn decode_var(buf: &mut &[u8]) -> Result<Self, DecodeError> {
        read_var_int(buf).map(|v| v as u32)
    }
}

impl EncodeVar for i64 {
    fn encode_var(&self, buf: &mut Vec<u8>) {
        write_var_long(buf, *self);
    }
}

impl DecodeVar for i64 {
    fn decode_var(buf: &mut &[u8]) -> Result<Self, DecodeError> {
        read_var_long(buf)
    }
}

impl EncodeVar for u64 {
    fn encode_var(&self, buf: &mut Vec<u8>) {
        write_var_long(buf, *self as i64);
    }
}

impl DecodeVar for u64 {
    fn decode_var(buf: &mut &[u8]) -> Result<Self, DecodeError> {
        read_var_long(buf).map(|v| v as u64)
    }
}

impl<T: EncodeVar> EncodeVar for Option<T> {
    fn encode_var(&self, buf: &mut Vec<u8>) {
        self.is_some().encode(buf);
        if let Some(v) = self {
            v.encode_var(buf);
        }
    }
}

impl<T: DecodeVar> DecodeVar for Option<T> {
    fn decode_var(buf: &mut &[u8]) -> Result<Self, DecodeError> {
        Ok(if bool::decode(buf)? { Some(T::decode_var(buf)?) } else { None })
    }
}

impl<T: EncodeVar> EncodeVar for Vec<T> {
    fn encode_var(&self, buf: &mut Vec<u8>) {
        write_var_int(buf, self.len() as i32);
        for v in self {
            v.encode_var(buf);
        }
    }
}

impl<T: DecodeVar> DecodeVar for Vec<T> {
    fn decode_var(buf: &mut &[u8]) -> Result<Self, DecodeError> {
        let len = crate::primitives::read_len(buf)?;
        // Every element is at least one byte, so this bounds preallocation.
        let mut out = Vec::with_capacity(len.min(buf.len()));
        for _ in 0..len {
            out.push(T::decode_var(buf)?);
        }
        Ok(out)
    }
}
