use uuid::Uuid;

use crate::var::{read_var_int, write_var_int};
use crate::{Decode, DecodeError, Encode, MAX_STRING_LEN, take};

macro_rules! impl_be {
    ($($ty:ty),*) => {$(
        impl Encode for $ty {
            fn encode(&self, buf: &mut Vec<u8>) {
                buf.extend_from_slice(&self.to_be_bytes());
            }
        }

        impl Decode for $ty {
            fn decode(buf: &mut &[u8]) -> Result<Self, DecodeError> {
                let bytes = take(buf, size_of::<$ty>())?;
                Ok(<$ty>::from_be_bytes(bytes.try_into().unwrap()))
            }
        }
    )*};
}

impl_be!(u8, i8, u16, i16, u32, i32, u64, i64, u128, f32, f64);

impl Encode for bool {
    fn encode(&self, buf: &mut Vec<u8>) {
        buf.push(*self as u8);
    }
}

impl Decode for bool {
    fn decode(buf: &mut &[u8]) -> Result<Self, DecodeError> {
        // Netty's readBoolean: any nonzero byte is true.
        Ok(take(buf, 1)?[0] != 0)
    }
}

pub(crate) fn read_len(buf: &mut &[u8]) -> Result<usize, DecodeError> {
    let len = read_var_int(buf)?;
    usize::try_from(len).map_err(|_| DecodeError::NegativeLength(len))
}

/// Mirrors `Utf8String.read`: the byte length may be at most 3x the limit,
/// and the decoded string at most `max` UTF-16 code units.
pub(crate) fn decode_string(buf: &mut &[u8], max: usize) -> Result<String, DecodeError> {
    let len = read_len(buf)?;
    let max_bytes = max * 3;
    if len > max_bytes {
        return Err(DecodeError::StringTooLong { len, max: max_bytes });
    }
    let s = std::str::from_utf8(take(buf, len)?).map_err(|_| DecodeError::InvalidUtf8)?;
    let utf16_len = s.encode_utf16().count();
    if utf16_len > max {
        return Err(DecodeError::StringTooLong { len: utf16_len, max });
    }
    Ok(s.to_owned())
}

impl Encode for str {
    fn encode(&self, buf: &mut Vec<u8>) {
        write_var_int(buf, self.len() as i32);
        buf.extend_from_slice(self.as_bytes());
    }
}

impl Encode for String {
    fn encode(&self, buf: &mut Vec<u8>) {
        self.as_str().encode(buf);
    }
}

impl Decode for String {
    fn decode(buf: &mut &[u8]) -> Result<Self, DecodeError> {
        decode_string(buf, MAX_STRING_LEN)
    }
}

impl Encode for Uuid {
    fn encode(&self, buf: &mut Vec<u8>) {
        self.as_u128().encode(buf);
    }
}

impl Decode for Uuid {
    fn decode(buf: &mut &[u8]) -> Result<Self, DecodeError> {
        u128::decode(buf).map(Uuid::from_u128)
    }
}

impl<T: Encode> Encode for Option<T> {
    fn encode(&self, buf: &mut Vec<u8>) {
        self.is_some().encode(buf);
        if let Some(v) = self {
            v.encode(buf);
        }
    }
}

impl<T: Decode> Decode for Option<T> {
    fn decode(buf: &mut &[u8]) -> Result<Self, DecodeError> {
        Ok(if bool::decode(buf)? { Some(T::decode(buf)?) } else { None })
    }
}

impl<T: Encode> Encode for [T] {
    fn encode(&self, buf: &mut Vec<u8>) {
        write_var_int(buf, self.len() as i32);
        for v in self {
            v.encode(buf);
        }
    }
}

impl<T: Encode> Encode for Vec<T> {
    fn encode(&self, buf: &mut Vec<u8>) {
        self.as_slice().encode(buf);
    }
}

impl<T: Decode> Decode for Vec<T> {
    fn decode(buf: &mut &[u8]) -> Result<Self, DecodeError> {
        let len = read_len(buf)?;
        // Bound preallocation by what the buffer could possibly hold.
        let mut out = Vec::with_capacity(len.min(buf.len()));
        for _ in 0..len {
            out.push(T::decode(buf)?);
        }
        Ok(out)
    }
}

/// Fixed-size arrays have no length prefix.
impl<T: Encode, const N: usize> Encode for [T; N] {
    fn encode(&self, buf: &mut Vec<u8>) {
        for v in self {
            v.encode(buf);
        }
    }
}

impl<T: Decode, const N: usize> Decode for [T; N] {
    fn decode(buf: &mut &[u8]) -> Result<Self, DecodeError> {
        let mut out = Vec::with_capacity(N);
        for _ in 0..N {
            out.push(T::decode(buf)?);
        }
        Ok(out.try_into().unwrap_or_else(|_| unreachable!()))
    }
}

impl<T: Encode + ?Sized> Encode for &T {
    fn encode(&self, buf: &mut Vec<u8>) {
        (**self).encode(buf);
    }
}

impl<T: Encode + ?Sized> Encode for Box<T> {
    fn encode(&self, buf: &mut Vec<u8>) {
        (**self).encode(buf);
    }
}

impl<T: Decode> Decode for Box<T> {
    fn decode(buf: &mut &[u8]) -> Result<Self, DecodeError> {
        T::decode(buf).map(Box::new)
    }
}
