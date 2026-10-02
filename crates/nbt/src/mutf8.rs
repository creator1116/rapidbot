//! Java's modified UTF-8 (`DataOutput.writeUTF`): UTF-16 code units encoded
//! individually, NUL as `C0 80`, supplementary characters as two 3-byte
//! surrogates.

use rapidbot_buf::DecodeError;

pub fn encode_mutf8(s: &str) -> Vec<u8> {
    let mut out = Vec::with_capacity(s.len());
    for unit in s.encode_utf16() {
        match unit {
            0x0001..=0x007f => out.push(unit as u8),
            0x0000 | 0x0080..=0x07ff => {
                out.push(0xc0 | (unit >> 6) as u8);
                out.push(0x80 | (unit & 0x3f) as u8);
            }
            _ => {
                out.push(0xe0 | (unit >> 12) as u8);
                out.push(0x80 | ((unit >> 6) & 0x3f) as u8);
                out.push(0x80 | (unit & 0x3f) as u8);
            }
        }
    }
    out
}

pub fn decode_mutf8(bytes: &[u8]) -> Result<String, DecodeError> {
    // Fast path: pure ASCII without NUL is identical in both encodings.
    if bytes.iter().all(|&b| b != 0 && b < 0x80) {
        return Ok(String::from_utf8(bytes.to_vec()).unwrap());
    }
    let bad = || DecodeError::Custom("malformed modified UTF-8".into());
    let mut units = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        let a = bytes[i] as u16;
        let (unit, len) = match a >> 4 {
            0..=7 => (a, 1),
            12 | 13 => {
                let b = *bytes.get(i + 1).ok_or_else(bad)? as u16;
                if b & 0xc0 != 0x80 {
                    return Err(bad());
                }
                (((a & 0x1f) << 6) | (b & 0x3f), 2)
            }
            14 => {
                let b = *bytes.get(i + 1).ok_or_else(bad)? as u16;
                let c = *bytes.get(i + 2).ok_or_else(bad)? as u16;
                if b & 0xc0 != 0x80 || c & 0xc0 != 0x80 {
                    return Err(bad());
                }
                (((a & 0x0f) << 12) | ((b & 0x3f) << 6) | (c & 0x3f), 3)
            }
            _ => return Err(bad()),
        };
        units.push(unit);
        i += len;
    }
    // Java strings may hold unpaired surrogates; Rust strings cannot.
    Ok(String::from_utf16_lossy(&units))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn java_specifics() {
        assert_eq!(encode_mutf8("\0"), [0xc0, 0x80]);
        assert_eq!(encode_mutf8("é"), [0xc3, 0xa9]);
        // U+1F600 is a surrogate pair, each surrogate 3 bytes.
        assert_eq!(encode_mutf8("😀"), [0xed, 0xa0, 0xbd, 0xed, 0xb8, 0x80]);
        for s in ["", "abc", "\0x\0", "é€😀 mixed"] {
            assert_eq!(decode_mutf8(&encode_mutf8(s)).unwrap(), s);
        }
    }
}
