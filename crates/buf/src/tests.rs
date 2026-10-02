use uuid::Uuid;

use crate::*;

fn roundtrip<T: Encode + Decode + PartialEq + std::fmt::Debug>(value: T) -> Vec<u8> {
    let bytes = to_bytes(&value);
    let mut slice = bytes.as_slice();
    assert_eq!(T::decode(&mut slice).unwrap(), value);
    assert!(slice.is_empty(), "trailing bytes");
    bytes
}

#[test]
fn var_int_matches_protocol_examples() {
    // Examples from the protocol documentation.
    let cases: &[(i32, &[u8])] = &[
        (0, &[0x00]),
        (1, &[0x01]),
        (127, &[0x7f]),
        (128, &[0x80, 0x01]),
        (255, &[0xff, 0x01]),
        (25565, &[0xdd, 0xc7, 0x01]),
        (2097151, &[0xff, 0xff, 0x7f]),
        (i32::MAX, &[0xff, 0xff, 0xff, 0xff, 0x07]),
        (-1, &[0xff, 0xff, 0xff, 0xff, 0x0f]),
        (i32::MIN, &[0x80, 0x80, 0x80, 0x80, 0x08]),
    ];
    for &(value, expected) in cases {
        assert_eq!(roundtrip(VarInt(value)), expected, "{value}");
        assert_eq!(var_int_len(value), expected.len(), "{value}");
    }
}

#[test]
fn var_long_matches_protocol_examples() {
    assert_eq!(roundtrip(VarLong(i64::MAX)), [0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0x7f]);
    assert_eq!(roundtrip(VarLong(-1)), [0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0x01]);
    assert_eq!(roundtrip(VarLong(i64::MIN)), [0x80, 0x80, 0x80, 0x80, 0x80, 0x80, 0x80, 0x80, 0x80, 0x01]);
}

#[test]
fn var_int_too_long_is_rejected() {
    let mut buf: &[u8] = &[0xff, 0xff, 0xff, 0xff, 0xff, 0x01];
    assert_eq!(VarInt::decode(&mut buf), Err(DecodeError::VarIntTooBig));
}

#[test]
fn strings() {
    assert_eq!(roundtrip("hello".to_string()), b"\x05hello");
    // Length prefix counts UTF-8 bytes.
    assert_eq!(roundtrip("é".to_string()), [0x02, 0xc3, 0xa9]);

    let long = "a".repeat(17);
    let buf = to_bytes(&long);
    let mut slice = buf.as_slice();
    assert!(matches!(BoundedString::<16>::decode(&mut slice), Err(DecodeError::StringTooLong { .. })));
}

#[test]
fn uuid_is_two_big_endian_longs() {
    let id = Uuid::parse_str("069a79f4-44e9-4726-a5be-fca90e38aaf5").unwrap();
    assert_eq!(
        roundtrip(id),
        [0x06, 0x9a, 0x79, 0xf4, 0x44, 0xe9, 0x47, 0x26, 0xa5, 0xbe, 0xfc, 0xa9, 0x0e, 0x38, 0xaa, 0xf5]
    );
}

#[test]
fn bool_decodes_like_netty() {
    let mut buf: &[u8] = &[0x02];
    assert!(bool::decode(&mut buf).unwrap());
}

#[derive(Debug, PartialEq, Encode, Decode)]
struct Example {
    #[var]
    id: i32,
    name: String,
    pos: (f64, f64, f64),
    #[var]
    ids: Vec<i32>,
    maybe: Option<u8>,
}

// Tuples are handy for vectors; keep them out of the public API until needed.
impl Encode for (f64, f64, f64) {
    fn encode(&self, buf: &mut Vec<u8>) {
        self.0.encode(buf);
        self.1.encode(buf);
        self.2.encode(buf);
    }
}

impl Decode for (f64, f64, f64) {
    fn decode(buf: &mut &[u8]) -> Result<Self, DecodeError> {
        Ok((f64::decode(buf)?, f64::decode(buf)?, f64::decode(buf)?))
    }
}

#[test]
fn derive_struct() {
    let bytes = roundtrip(Example { id: 300, name: "a".into(), pos: (1.0, 2.0, 3.0), ids: vec![1, 128], maybe: None });
    assert_eq!(&bytes[..2], &[0xac, 0x02]);
    assert_eq!(*bytes.last().unwrap(), 0);
}

#[derive(Debug, PartialEq, Encode, Decode)]
enum Intent {
    Status = 1,
    Login,
    Transfer,
}

#[derive(Debug, PartialEq, Encode, Decode)]
#[discriminant(u8)]
enum Hand {
    Main,
    Off,
}

#[derive(Debug, PartialEq, Encode, Decode)]
enum Action {
    Start { #[var] sequence: i32 },
    Stop(u8),
    Nothing,
}

#[test]
fn derive_enums() {
    assert_eq!(roundtrip(Intent::Status), [1]);
    assert_eq!(roundtrip(Intent::Transfer), [3]);
    assert_eq!(roundtrip(Hand::Off), [1]);
    assert_eq!(roundtrip(Action::Start { sequence: 5 }), [0, 5]);
    assert_eq!(roundtrip(Action::Stop(9)), [1, 9]);
    assert_eq!(roundtrip(Action::Nothing), [2]);

    let mut buf: &[u8] = &[7];
    assert_eq!(
        Intent::decode(&mut buf),
        Err(DecodeError::InvalidDiscriminant { ty: "Intent", value: 7 })
    );
}
