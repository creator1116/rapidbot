//! Network NBT (`NbtIo.writeAnyTag` / `readAnyTag`): a type byte followed by
//! the payload, with no root name. Strings use Java's modified UTF-8.

mod mutf8;

use rapidbot_buf::{Decode, DecodeError, Encode, take};

pub use mutf8::{decode_mutf8, encode_mutf8};

/// `NbtAccounter.MAX_STACK_DEPTH`.
pub const MAX_DEPTH: usize = 512;

pub mod id {
    pub const END: u8 = 0;
    pub const BYTE: u8 = 1;
    pub const SHORT: u8 = 2;
    pub const INT: u8 = 3;
    pub const LONG: u8 = 4;
    pub const FLOAT: u8 = 5;
    pub const DOUBLE: u8 = 6;
    pub const BYTE_ARRAY: u8 = 7;
    pub const STRING: u8 = 8;
    pub const LIST: u8 = 9;
    pub const COMPOUND: u8 = 10;
    pub const INT_ARRAY: u8 = 11;
    pub const LONG_ARRAY: u8 = 12;
}

#[derive(Debug, Clone, PartialEq)]
pub enum Tag {
    Byte(i8),
    Short(i16),
    Int(i32),
    Long(i64),
    Float(f32),
    Double(f64),
    ByteArray(Vec<u8>),
    String(String),
    /// Every element has the same type, as on the wire.
    List(Vec<Tag>),
    Compound(Compound),
    IntArray(Vec<i32>),
    LongArray(Vec<i64>),
}

/// Entries in wire order. Vanilla's `CompoundTag` is a `HashMap`, so the
/// order it writes in is Java's hash iteration order; keep that in mind when
/// re-encoding something the server will compare byte for byte.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Compound(pub Vec<(String, Tag)>);

impl Compound {
    pub fn get(&self, key: &str) -> Option<&Tag> {
        self.0.iter().find(|(k, _)| k == key).map(|(_, v)| v)
    }

    pub fn insert(&mut self, key: impl Into<String>, value: Tag) {
        let key = key.into();
        match self.0.iter_mut().find(|(k, _)| *k == key) {
            Some((_, v)) => *v = value,
            None => self.0.push((key, value)),
        }
    }

    pub fn iter(&self) -> impl Iterator<Item = (&str, &Tag)> {
        self.0.iter().map(|(k, v)| (k.as_str(), v))
    }
}

impl Tag {
    pub fn id(&self) -> u8 {
        match self {
            Tag::Byte(_) => id::BYTE,
            Tag::Short(_) => id::SHORT,
            Tag::Int(_) => id::INT,
            Tag::Long(_) => id::LONG,
            Tag::Float(_) => id::FLOAT,
            Tag::Double(_) => id::DOUBLE,
            Tag::ByteArray(_) => id::BYTE_ARRAY,
            Tag::String(_) => id::STRING,
            Tag::List(_) => id::LIST,
            Tag::Compound(_) => id::COMPOUND,
            Tag::IntArray(_) => id::INT_ARRAY,
            Tag::LongArray(_) => id::LONG_ARRAY,
        }
    }

    pub fn as_str(&self) -> Option<&str> {
        match self {
            Tag::String(s) => Some(s),
            _ => None,
        }
    }

    pub fn as_compound(&self) -> Option<&Compound> {
        match self {
            Tag::Compound(c) => Some(c),
            _ => None,
        }
    }

    pub fn as_list(&self) -> Option<&[Tag]> {
        match self {
            Tag::List(l) => Some(l),
            _ => None,
        }
    }

    /// Numeric tags as i64, the way `NumericTag` coerces.
    pub fn as_i64(&self) -> Option<i64> {
        Some(match *self {
            Tag::Byte(v) => v as i64,
            Tag::Short(v) => v as i64,
            Tag::Int(v) => v as i64,
            Tag::Long(v) => v,
            Tag::Float(v) => v as i64,
            Tag::Double(v) => v as i64,
            _ => return None,
        })
    }

    /// Writes the payload only (no type byte).
    pub fn write_payload(&self, buf: &mut Vec<u8>) {
        match self {
            Tag::Byte(v) => v.encode(buf),
            Tag::Short(v) => v.encode(buf),
            Tag::Int(v) => v.encode(buf),
            Tag::Long(v) => v.encode(buf),
            Tag::Float(v) => v.encode(buf),
            Tag::Double(v) => v.encode(buf),
            Tag::ByteArray(v) => {
                (v.len() as i32).encode(buf);
                buf.extend_from_slice(v);
            }
            Tag::String(s) => write_string(buf, s),
            Tag::List(items) => {
                // ListTag writes End as the element type of an empty list.
                buf.push(items.first().map_or(id::END, Tag::id));
                (items.len() as i32).encode(buf);
                for item in items {
                    debug_assert_eq!(item.id(), items[0].id(), "mixed list");
                    item.write_payload(buf);
                }
            }
            Tag::Compound(c) => {
                for (k, v) in &c.0 {
                    buf.push(v.id());
                    write_string(buf, k);
                    v.write_payload(buf);
                }
                buf.push(id::END);
            }
            Tag::IntArray(v) => {
                (v.len() as i32).encode(buf);
                v.iter().for_each(|x| x.encode(buf));
            }
            Tag::LongArray(v) => {
                (v.len() as i32).encode(buf);
                v.iter().for_each(|x| x.encode(buf));
            }
        }
    }

    pub fn read_payload(ty: u8, buf: &mut &[u8], depth: usize) -> Result<Tag, DecodeError> {
        if depth > MAX_DEPTH {
            return Err(DecodeError::Custom("tried to read NBT tag with too high complexity, depth > 512".into()));
        }
        Ok(match ty {
            id::BYTE => Tag::Byte(i8::decode(buf)?),
            id::SHORT => Tag::Short(i16::decode(buf)?),
            id::INT => Tag::Int(i32::decode(buf)?),
            id::LONG => Tag::Long(i64::decode(buf)?),
            id::FLOAT => Tag::Float(f32::decode(buf)?),
            id::DOUBLE => Tag::Double(f64::decode(buf)?),
            id::BYTE_ARRAY => {
                let len = read_len(buf)?;
                Tag::ByteArray(take(buf, len)?.to_vec())
            }
            id::STRING => Tag::String(read_string(buf)?),
            id::LIST => {
                let elem = u8::decode(buf)?;
                let len = read_len(buf)?;
                if elem == id::END && len > 0 {
                    return Err(DecodeError::Custom("missing type on ListTag".into()));
                }
                let mut items = Vec::with_capacity(len.min(buf.len()));
                for _ in 0..len {
                    items.push(Tag::read_payload(elem, buf, depth + 1)?);
                }
                Tag::List(items)
            }
            id::COMPOUND => {
                let mut c = Compound::default();
                loop {
                    let ty = u8::decode(buf)?;
                    if ty == id::END {
                        break;
                    }
                    let key = read_string(buf)?;
                    let value = Tag::read_payload(ty, buf, depth + 1)?;
                    c.insert(key, value);
                }
                Tag::Compound(c)
            }
            id::INT_ARRAY => {
                let len = read_len(buf)?;
                let mut v = Vec::with_capacity(len.min(buf.len() / 4));
                for _ in 0..len {
                    v.push(i32::decode(buf)?);
                }
                Tag::IntArray(v)
            }
            id::LONG_ARRAY => {
                let len = read_len(buf)?;
                let mut v = Vec::with_capacity(len.min(buf.len() / 8));
                for _ in 0..len {
                    v.push(i64::decode(buf)?);
                }
                Tag::LongArray(v)
            }
            other => return Err(DecodeError::Custom(format!("invalid NBT tag type {other}"))),
        })
    }
}

fn read_len(buf: &mut &[u8]) -> Result<usize, DecodeError> {
    let len = i32::decode(buf)?;
    usize::try_from(len).map_err(|_| DecodeError::NegativeLength(len))
}

fn write_string(buf: &mut Vec<u8>, s: &str) {
    let bytes = encode_mutf8(s);
    debug_assert!(bytes.len() <= u16::MAX as usize, "NBT string too long");
    (bytes.len() as u16).encode(buf);
    buf.extend_from_slice(&bytes);
}

fn read_string(buf: &mut &[u8]) -> Result<String, DecodeError> {
    let len = u16::decode(buf)? as usize;
    decode_mutf8(take(buf, len)?)
}

/// Reads a network tag; `None` for an End tag (vanilla's null).
pub fn read_network(buf: &mut &[u8]) -> Result<Option<Tag>, DecodeError> {
    let ty = u8::decode(buf)?;
    if ty == id::END {
        return Ok(None);
    }
    Tag::read_payload(ty, buf, 0).map(Some)
}

pub fn write_network(buf: &mut Vec<u8>, tag: Option<&Tag>) {
    match tag {
        None => buf.push(id::END),
        Some(tag) => {
            buf.push(tag.id());
            tag.write_payload(buf);
        }
    }
}

/// A non-null network tag (`ByteBufCodecs.TAG`).
impl Encode for Tag {
    fn encode(&self, buf: &mut Vec<u8>) {
        write_network(buf, Some(self));
    }
}

impl Decode for Tag {
    fn decode(buf: &mut &[u8]) -> Result<Self, DecodeError> {
        read_network(buf)?.ok_or_else(|| DecodeError::Custom("expected non-null NBT tag".into()))
    }
}

/// A nullable network tag (`FriendlyByteBuf.writeNbt` with null).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct OptionalTag(pub Option<Tag>);

impl Encode for OptionalTag {
    fn encode(&self, buf: &mut Vec<u8>) {
        write_network(buf, self.0.as_ref());
    }
}

impl Decode for OptionalTag {
    fn decode(buf: &mut &[u8]) -> Result<Self, DecodeError> {
        read_network(buf).map(Self)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip_nested() {
        let mut inner = Compound::default();
        inner.insert("name", Tag::String("Bananrama".into()));
        let mut root = Compound::default();
        root.insert("byte", Tag::Byte(-1));
        root.insert("list", Tag::List(vec![Tag::Int(1), Tag::Int(2)]));
        root.insert("empty", Tag::List(vec![]));
        root.insert("longs", Tag::LongArray(vec![i64::MIN, 0]));
        root.insert("inner", Tag::Compound(inner));
        let tag = Tag::Compound(root);

        let bytes = rapidbot_buf::to_bytes(&tag);
        let mut slice = bytes.as_slice();
        assert_eq!(Tag::decode(&mut slice).unwrap(), tag);
        assert!(slice.is_empty());
    }

    #[test]
    fn string_root_and_null() {
        // Text components are often a bare string tag.
        let bytes = rapidbot_buf::to_bytes(&Tag::String("hi".into()));
        assert_eq!(bytes, [8, 0, 2, b'h', b'i']);
        let mut end: &[u8] = &[0];
        assert_eq!(OptionalTag::decode(&mut end).unwrap(), OptionalTag(None));
    }

    #[test]
    fn depth_limit() {
        // 600 nested lists of lists.
        let mut bytes = vec![id::LIST];
        for _ in 0..600 {
            bytes.extend_from_slice(&[id::LIST, 0, 0, 0, 1]);
        }
        bytes.extend_from_slice(&[id::END, 0, 0, 0, 0]);
        assert!(read_network(&mut bytes.as_slice()).is_err());
    }
}
