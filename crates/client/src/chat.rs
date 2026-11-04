//! Chat as the vanilla client keeps it: which signed messages it has seen
//! (`LastSeenMessagesTracker`), the signature cache player chat packets are
//! packed against (`MessageSignatureCache`), and the signing chain for its
//! own messages (`SignedMessageChain`).

use std::collections::HashMap;

use rapidbot_buf::{Decode, DecodeError, VarInt};
use rapidbot_protocol::packets::play::serverbound as sb;
pub use rapidbot_protocol::packets::play::serverbound::LastSeenUpdate;
use rsa::RsaPublicKey;
use rsa::pkcs1v15::{Signature as RsaSignature, SigningKey, VerifyingKey};
use rsa::pkcs8::{DecodePrivateKey, DecodePublicKey};
use rsa::sha2::Sha256;
use rsa::signature::{SignatureEncoding, Signer, Verifier};
use uuid::Uuid;

use crate::entities::PlayerInfo;

pub const SIGNATURE_BYTES: usize = 256;
pub type Signature = [u8; SIGNATURE_BYTES];

/// The sender's `RemoteChatSession.Data` from `player_info_update`: the
/// session ID and profile key a signed chat message is checked against.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChatSession {
    pub session_id: Uuid,
    /// Unix milliseconds.
    pub expires_at: i64,
    /// X.509 SubjectPublicKeyInfo DER.
    pub public_key_der: Vec<u8>,
}

/// `ProfilePublicKey.EXPIRY_GRACE_PERIOD`.
const PROFILE_KEY_GRACE_MS: i64 = 8 * 60 * 60 * 1000;

/// `SignedMessageValidator.KeyBased`: one per sender and chat session.
/// Once a message fails, every later message from that session is rejected
/// until the sender initializes a new session.
#[derive(Debug, Clone)]
struct SenderValidator {
    session_id: Uuid,
    expires_at: i64,
    public_key_der: Vec<u8>,
    last_index: Option<i32>,
    last_signature: Option<Signature>,
    chain_valid: bool,
}

impl SenderValidator {
    fn new(session: &ChatSession) -> Self {
        Self {
            session_id: session.session_id,
            expires_at: session.expires_at,
            public_key_der: session.public_key_der.clone(),
            last_index: None,
            last_signature: None,
            chain_valid: true,
        }
    }

    fn same_session(&self, session: &ChatSession) -> bool {
        self.session_id == session.session_id && self.public_key_der == session.public_key_der
    }
}

/// `LastSeenMessages.LAST_SEEN_MESSAGES_MAX_LENGTH`.
const LAST_SEEN: usize = 20;

/// `FilterMask`: how much of a chat message the server filtered out.
enum FilterMask {
    PassThrough,
    FullyFiltered,
    /// `BitSet.valueOf` bytes; bit `i` masks UTF-16 unit `i`.
    Partial(Vec<u8>),
}

impl FilterMask {
    /// `FilterMask.STREAM_CODEC`: a type id, then the bitset for a partial
    /// mask. Unknown ids map to pass-through (`ByIdMap` fallback ZERO).
    fn decode(buf: &mut &[u8]) -> Result<Self, DecodeError> {
        Ok(match VarInt::decode(buf)?.0 {
            1 => Self::FullyFiltered,
            2 => Self::Partial(rapidbot_buf::ByteArray::decode(buf)?.0),
            _ => Self::PassThrough,
        })
    }

    /// The message as `ChatListener.showMessageToPlayer` renders it, or
    /// `None` when the message is fully filtered. A pass-through mask shows
    /// the unsigned content when the server sent any, while a partial mask
    /// masks the signed content (`FilterMask.applyWithFormatting`).
    fn display(&self, signed: &str, unsigned: Option<&rapidbot_nbt::Tag>) -> Option<String> {
        match self {
            Self::PassThrough => Some(match unsigned {
                Some(content) => crate::text::nbt_to_plain(content),
                None => signed.to_owned(),
            }),
            Self::FullyFiltered => None,
            Self::Partial(bits) => Some(masked(signed, bits)),
        }
    }
}

/// `FilterMask.apply`: masked characters are shown as `#`.
fn masked(text: &str, bits: &[u8]) -> String {
    let mut out = String::with_capacity(text.len());
    let mut unit = 0usize;
    for c in text.chars() {
        if bits
            .get(unit >> 3)
            .is_some_and(|b| b & (1 << (unit & 7)) != 0)
        {
            out.push('#');
        } else {
            out.push(c);
        }
        unit += c.len_utf16();
    }
    out
}

/// `MessageSignatureCache.DEFAULT_CAPACITY`.
const CACHE_SIZE: usize = 128;
/// `ChatScreen`/`ServerboundChatPacket`: 256 UTF-16 units.
pub const MAX_MESSAGE_LENGTH: usize = 256;

/// `MessageSignature.checksum()`: `Arrays.hashCode(bytes)`.
fn checksum(signature: &Signature) -> i32 {
    signature.iter().fold(1i32, |h, &b| {
        h.wrapping_mul(31).wrapping_add(b as i8 as i32)
    })
}

/// `LastSeenMessages.computeChecksum`.
fn last_seen_checksum(entries: &[Signature]) -> u8 {
    let sum = entries
        .iter()
        .fold(1i32, |h, e| h.wrapping_mul(31).wrapping_add(checksum(e)));
    match sum as u8 {
        0 => 1,
        b => b,
    }
}

/// `LastSeenMessagesTracker`.
#[derive(Debug, Clone)]
struct Tracker {
    /// Signature and whether it is still pending (not yet acknowledged).
    tracked: [Option<(Signature, bool)>; LAST_SEEN],
    tail: usize,
    offset: i32,
    last_tracked: Option<Signature>,
}

impl Tracker {
    fn new() -> Self {
        Self {
            tracked: [None; LAST_SEEN],
            tail: 0,
            offset: 0,
            last_tracked: None,
        }
    }

    /// `addPending`.
    fn add_pending(&mut self, signature: Signature, was_shown: bool) -> bool {
        if self.last_tracked == Some(signature) {
            return false;
        }
        self.last_tracked = Some(signature);
        self.tracked[self.tail] = was_shown.then_some((signature, true));
        self.tail = (self.tail + 1) % LAST_SEEN;
        self.offset += 1;
        true
    }

    /// `ignorePending`.
    fn ignore_pending(&mut self, signature: &Signature) {
        if let Some(slot) = self
            .tracked
            .iter_mut()
            .find(|s| matches!(s, Some((sig, true)) if sig == signature))
        {
            *slot = None;
        }
    }

    fn take_offset(&mut self) -> i32 {
        std::mem::take(&mut self.offset)
    }

    /// `generateAndApplyUpdate`: the seen messages, oldest first, and the
    /// update describing them.
    fn generate_update(&mut self) -> (Vec<Signature>, LastSeenUpdate) {
        let offset = self.take_offset();
        let mut acknowledged = 0u32;
        let mut entries = Vec::new();
        for i in 0..LAST_SEEN {
            let index = (self.tail + i) % LAST_SEEN;
            if let Some((signature, pending)) = &mut self.tracked[index] {
                acknowledged |= 1 << i;
                entries.push(*signature);
                *pending = false;
            }
        }
        let checksum = last_seen_checksum(&entries);
        (
            entries,
            LastSeenUpdate {
                offset,
                acknowledged,
                checksum,
            },
        )
    }
}

/// `MessageSignatureCache`.
#[derive(Debug, Clone)]
struct SignatureCache {
    entries: Vec<Option<Signature>>,
}

impl SignatureCache {
    fn new() -> Self {
        Self {
            entries: vec![None; CACHE_SIZE],
        }
    }

    /// `push(body, signature)`: the message's last-seen list, then its own
    /// signature, go to the front; what they displace shifts back.
    fn push(&mut self, last_seen: &[Signature], signature: Option<&Signature>) {
        let mut queue: std::collections::VecDeque<Signature> = last_seen.iter().copied().collect();
        if let Some(signature) = signature {
            queue.push_back(*signature);
        }
        let new: Vec<Signature> = queue.iter().copied().collect();
        let mut i = 0;
        while let Some(next) = (i < self.entries.len()).then(|| queue.pop_back()).flatten() {
            let old = self.entries[i].replace(next);
            if let Some(old) = old
                && !new.contains(&old)
            {
                queue.push_front(old);
            }
            i += 1;
        }
    }

    /// `MessageSignature.Packed.read` + `unpack`.
    fn read(&self, buf: &mut &[u8]) -> Result<Option<Signature>, DecodeError> {
        let id = VarInt::decode(buf)?.0 - 1;
        if id == -1 {
            return Ok(Some(read_signature(buf)?));
        }
        Ok(self.entries.get(id as usize).copied().flatten())
    }
}

fn read_signature(buf: &mut &[u8]) -> Result<Signature, DecodeError> {
    let mut signature = [0u8; SIGNATURE_BYTES];
    signature.copy_from_slice(rapidbot_buf::take(buf, SIGNATURE_BYTES)?);
    Ok(signature)
}

/// `LocalChatSession` with its `SignedMessageChain.Encoder`.
struct Session {
    key: SigningKey<Sha256>,
    sender: Uuid,
    session_id: Uuid,
    /// `SignedMessageLink.index` of the next message.
    index: i32,
}

/// A player chat message as far as the bot cares.
#[derive(Debug, Clone, PartialEq)]
pub struct Incoming {
    pub sender: Uuid,
    pub text: String,
}

/// One `player_chat` packet after validation. `incoming` is `None` for an
/// unknown sender, an invalid signature, or a fully filtered message; those
/// are not shown but still count towards the last-seen acknowledgement when
/// they carried a signature, as `handleChatMessageError` does.
#[derive(Debug, Clone, PartialEq)]
pub struct ProcessedChat {
    pub incoming: Option<Incoming>,
    /// `chat_ack` to send: more than 64 messages seen since the last
    /// message or acknowledgement of ours.
    pub ack: Option<i32>,
}

/// A chat line ready to go out.
#[derive(Debug, Clone, PartialEq)]
pub enum Outgoing {
    Chat(sb::Chat),
    Command(sb::ChatCommand),
    SignedCommand(sb::ChatCommandSigned),
}

#[derive(Debug, thiserror::Error)]
pub enum ChatError {
    #[error("bad chat packet: {0}")]
    Decode(#[from] DecodeError),
    /// Vanilla disconnects with `multiplayer.disconnect.bad_chat_index`.
    #[error("missing or out-of-order chat message: expected index {expected}, got {got}")]
    BadIndex { expected: i32, got: i32 },
    /// Vanilla disconnects with `multiplayer.disconnect.invalid_packet`.
    #[error("chat packet referenced an unknown signature")]
    UnknownSignature,
}

pub struct ChatState {
    tracker: Tracker,
    cache: SignatureCache,
    next_chat_index: i32,
    session: Option<Session>,
    /// The per-sender `SignedMessageValidator.KeyBased` chain state.
    validators: HashMap<Uuid, SenderValidator>,
}

impl ChatState {
    /// A fresh listener's chat state: nothing seen, messages unsigned.
    pub fn new() -> Self {
        Self {
            tracker: Tracker::new(),
            cache: SignatureCache::new(),
            next_chat_index: 0,
            session: None,
            validators: HashMap::new(),
        }
    }

    /// `LocalChatSession.create`: from now on messages are signed with the
    /// profile key. Returns the random session ID for `chat_session_update`,
    /// or `None` if the key cannot be read.
    pub fn start_session(&mut self, sender: Uuid, private_key_pem: &str) -> Option<Uuid> {
        // Mojang labels its PKCS#8 key "RSA PRIVATE KEY".
        let pem = private_key_pem.replace("RSA PRIVATE KEY", "PRIVATE KEY");
        let key = rsa::RsaPrivateKey::from_pkcs8_pem(&pem).ok()?;
        let session_id = Uuid::new_v4();
        self.session = Some(Session {
            key: SigningKey::new(key),
            sender,
            session_id,
            index: 0,
        });
        Some(session_id)
    }

    /// `handlePlayerChat`, including the sender's `PlayerInfo` chat session
    /// and `SignedMessageValidator.KeyBased`. `sender_info` is the tab-list
    /// entry for the sender; `enforces_secure_chat` is the server's
    /// `enforcesSecureChat` flag.
    pub fn player_chat<'a>(
        &mut self,
        mut body: &[u8],
        sender_info: impl Fn(&Uuid) -> Option<&'a PlayerInfo>,
        enforces_secure_chat: bool,
    ) -> Result<ProcessedChat, ChatError> {
        let buf = &mut body;
        let global_index = VarInt::decode(buf)?.0;
        let expected = self.next_chat_index;
        self.next_chat_index += 1;
        if global_index != expected {
            return Err(ChatError::BadIndex {
                expected,
                got: global_index,
            });
        }
        let sender = Uuid::decode(buf)?;
        let index = VarInt::decode(buf)?.0;
        let signature = if bool::decode(buf)? {
            Some(read_signature(buf)?)
        } else {
            None
        };
        // SignedMessageBody.Packed
        let text = String::decode(buf)?;
        let timestamp = i64::decode(buf)?;
        let salt = i64::decode(buf)?;
        let count = VarInt::decode(buf)?.0;
        // `ByteBufCodecs.list(20)` rejects longer lists.
        if count > LAST_SEEN as i32 {
            return Err(DecodeError::Custom(format!(
                "{count} last-seen entries, max is {LAST_SEEN}"
            ))
            .into());
        }
        let mut last_seen = Vec::with_capacity(count.max(0) as usize);
        for _ in 0..count {
            last_seen.push(self.cache.read(buf)?.ok_or(ChatError::UnknownSignature)?);
        }
        let unsigned = if bool::decode(buf)? {
            Some(rapidbot_nbt::Tag::decode(buf)?)
        } else {
            None
        };
        let mask = FilterMask::decode(buf)?;

        self.cache.push(&last_seen, signature.as_ref());
        let info = sender_info(&sender);
        let mut incoming = None;
        let mut tracked = None;
        match info {
            None => {
                // Vanilla shows a chat-error line; the message is not chat.
                tracked = signature;
            }
            Some(info) => match &info.chat_session {
                None => {
                    // `PlayerInfo.clearChatSession`: no verifiable key.
                    self.validators.remove(&sender);
                    if enforces_secure_chat {
                        tracked = signature;
                    } else if let Some(text) = mask.display(&text, unsigned.as_ref()) {
                        // `SignedMessageValidator.ACCEPT_UNSIGNED` strips a
                        // signature and shows the content.
                        incoming = Some(Incoming { sender, text });
                    }
                }
                Some(session) => {
                    if self.validate_signed(
                        session,
                        sender,
                        index,
                        signature.as_ref(),
                        &text,
                        timestamp,
                        salt,
                        &last_seen,
                    ) {
                        tracked = signature;
                        if let Some(text) = mask.display(&text, unsigned.as_ref()) {
                            incoming = Some(Incoming { sender, text });
                        }
                    } else {
                        // `handleChatMessageError` still marks the invalid
                        // signature as processed so it is acknowledged.
                        tracked = signature;
                    }
                }
            },
        }

        let mut ack = None;
        if let Some(signature) = tracked {
            // ChatListener → markMessageAsProcessed.
            let shown = incoming.is_some();
            if self.tracker.add_pending(signature, shown) && self.tracker.offset > 64 {
                ack = Some(self.tracker.take_offset());
            }
        }
        Ok(ProcessedChat { incoming, ack })
    }

    /// `SignedMessageValidator.KeyBased.updateAndValidate`.
    #[allow(clippy::too_many_arguments)]
    fn validate_signed(
        &mut self,
        session: &ChatSession,
        sender: Uuid,
        index: i32,
        signature: Option<&Signature>,
        text: &str,
        timestamp: i64,
        salt: i64,
        last_seen: &[Signature],
    ) -> bool {
        let validator = self
            .validators
            .entry(sender)
            .or_insert_with(|| SenderValidator::new(session));
        if !validator.same_session(session) {
            *validator = SenderValidator::new(session);
        }
        if !validator.chain_valid {
            return false;
        }
        if validator.expires_at.saturating_add(PROFILE_KEY_GRACE_MS) < crate::clock::millis() {
            validator.chain_valid = false;
            return false;
        }
        let Some(signature) = signature else {
            validator.chain_valid = false;
            return false;
        };
        // The exact same message twice is allowed and does not advance the
        // chain. A different message at the same index is not a descendant.
        if validator.last_index == Some(index) {
            if validator.last_signature == Some(*signature) {
                return true;
            }
            validator.chain_valid = false;
            return false;
        }
        if validator.last_index.is_some_and(|last| index <= last) {
            validator.chain_valid = false;
            return false;
        }
        let data = signed_bytes(
            sender,
            session.session_id,
            index,
            text,
            timestamp,
            salt,
            last_seen,
        );
        if !verify_signature(&session.public_key_der, &data, signature) {
            validator.chain_valid = false;
            return false;
        }
        validator.last_index = Some(index);
        validator.last_signature = Some(*signature);
        true
    }

    /// `handleDeleteChat`.
    pub fn delete_chat(&mut self, mut body: &[u8]) -> Result<(), ChatError> {
        let signature = self
            .cache
            .read(&mut body)?
            .ok_or(ChatError::UnknownSignature)?;
        self.tracker.ignore_pending(&signature);
        Ok(())
    }

    /// `ClientPacketListener.sendChat`.
    pub fn chat(&mut self, message: &str, now_ms: i64) -> Outgoing {
        let salt: i64 = rand::random();
        let (seen, last_seen) = self.tracker.generate_update();
        let signature = self.sign(message, now_ms, salt, &seen);
        Outgoing::Chat(sb::Chat {
            message: message.to_owned(),
            timestamp: now_ms,
            salt,
            signature,
            last_seen,
        })
    }

    /// `ClientPacketListener.sendCommand` (without the leading slash).
    /// `signable` is what `CommandTree::signable_arguments` found.
    pub fn command(
        &mut self,
        command: &str,
        signable: &[(String, String)],
        now_ms: i64,
    ) -> Outgoing {
        if signable.is_empty() {
            return Outgoing::Command(sb::ChatCommand {
                command: command.to_owned(),
            });
        }
        let salt: i64 = rand::random();
        let (seen, last_seen) = self.tracker.generate_update();
        // ArgumentSignatures.signCommand: arguments that could not be
        // signed (no chat session) are left out.
        let signatures = signable
            .iter()
            .filter_map(|(name, value)| {
                Some((name.clone(), self.sign(value, now_ms, salt, &seen)?))
            })
            .collect();
        Outgoing::SignedCommand(sb::ChatCommandSigned {
            command: command.to_owned(),
            timestamp: now_ms,
            salt,
            signatures,
            last_seen,
        })
    }

    /// `SignedMessageChain.Encoder.pack`: `SHA256withRSA` over
    /// `PlayerChatMessage.updateSignature`.
    fn sign(
        &mut self,
        content: &str,
        now_ms: i64,
        salt: i64,
        last_seen: &[Signature],
    ) -> Option<Box<Signature>> {
        let session = self.session.as_mut()?;
        let data = signed_bytes(
            session.sender,
            session.session_id,
            session.index,
            content,
            now_ms,
            salt,
            last_seen,
        );
        session.index = session.index.checked_add(1)?;
        let signature = session.key.sign(&data).to_bytes();
        let mut out = Box::new([0u8; SIGNATURE_BYTES]);
        if signature.len() != SIGNATURE_BYTES {
            return None;
        }
        out.copy_from_slice(&signature);
        Some(out)
    }
}

impl Default for ChatState {
    fn default() -> Self {
        Self::new()
    }
}

/// `SignatureValidator.from(key, "SHA256withRSA")`.
fn verify_signature(public_key_der: &[u8], data: &[u8], signature: &Signature) -> bool {
    let Ok(key) = RsaPublicKey::from_public_key_der(public_key_der) else {
        return false;
    };
    let Ok(signature) = RsaSignature::try_from(signature.as_slice()) else {
        return false;
    };
    VerifyingKey::<Sha256>::new(key)
        .verify(data, &signature)
        .is_ok()
}

/// The bytes a chat signature covers: version 1, the link (sender, session,
/// index), then the body (salt, time in seconds, content, last seen).
fn signed_bytes(
    sender: Uuid,
    session_id: Uuid,
    index: i32,
    content: &str,
    now_ms: i64,
    salt: i64,
    last_seen: &[Signature],
) -> Vec<u8> {
    let mut data = Vec::with_capacity(64 + content.len() + last_seen.len() * SIGNATURE_BYTES);
    data.extend_from_slice(&1i32.to_be_bytes());
    data.extend_from_slice(sender.as_bytes());
    data.extend_from_slice(session_id.as_bytes());
    data.extend_from_slice(&index.to_be_bytes());
    data.extend_from_slice(&salt.to_be_bytes());
    // Instant.getEpochSecond floors.
    data.extend_from_slice(&now_ms.div_euclid(1000).to_be_bytes());
    data.extend_from_slice(&(content.len() as i32).to_be_bytes());
    data.extend_from_slice(content.as_bytes());
    data.extend_from_slice(&(last_seen.len() as i32).to_be_bytes());
    for signature in last_seen {
        data.extend_from_slice(signature);
    }
    data
}

/// `StringUtil.isAllowedChatCharacter`, the edit box's input filter.
fn allowed_chat_char(c: char) -> bool {
    let c = c as u32;
    c != 0xa7 && c >= 32 && c != 127
}

/// `Character.isWhitespace`: the Unicode space separators except the
/// non-breaking ones (`U+00A0`, `U+2007`, `U+202F`), plus the control
/// whitespace `U+0009..=U+000D` and `U+001C..=U+001F`.
fn is_java_whitespace(c: char) -> bool {
    matches!(
        c as u32,
        0x09..=0x0D
            | 0x1C..=0x1F
            | 0x20
            | 0x1680
            | 0x2000..=0x2006
            | 0x2008..=0x200A
            | 0x2028
            | 0x2029
            | 0x205F
            | 0x3000
    )
}

/// `ChatScreen.normalizeChatMessage` after the edit box's character filter:
/// `StringUtil.filterText`, then `StringUtils.normalizeSpace` (Java
/// `isWhitespace` runs collapse to one space, both ends stripped), then
/// `StringUtil.trimChatMessage` (at most 256 UTF-16 units). A surrogate
/// pair cut by the limit becomes `?` when the packet writes UTF-8.
pub fn normalize(message: &str) -> String {
    let filtered: String = message.chars().filter(|&c| allowed_chat_char(c)).collect();
    let mut collapsed = String::with_capacity(filtered.len());
    let mut last_was_space = true;
    for c in filtered.chars() {
        if is_java_whitespace(c) {
            if !last_was_space {
                collapsed.push(' ');
            }
            last_was_space = true;
        } else {
            collapsed.push(c);
            last_was_space = false;
        }
    }
    if last_was_space {
        // `StrBuilder.trim` of `normalizeSpace`.
        collapsed.pop();
    }
    let mut out = String::with_capacity(collapsed.len());
    let mut units = 0;
    for c in collapsed.chars() {
        let n = c.len_utf16();
        if units + n > MAX_MESSAGE_LENGTH {
            if n == 2 && units + 1 == MAX_MESSAGE_LENGTH {
                out.push('?');
            }
            break;
        }
        out.push(c);
        units += n;
    }
    out
}

#[cfg(test)]
mod tests {
    use rapidbot_buf::Encode;

    use super::*;

    fn sig(n: u8) -> Signature {
        [n; SIGNATURE_BYTES]
    }

    #[test]
    fn checksums_match_java() {
        // Arrays.hashCode(new byte[256]) with every byte 1, computed by the
        // same recurrence in 32-bit arithmetic.
        let mut h = 1i32;
        for _ in 0..256 {
            h = h.wrapping_mul(31).wrapping_add(1);
        }
        assert_eq!(checksum(&sig(1)), h);
        // Bytes are signed in Java.
        assert_eq!(
            checksum(&sig(0xff)),
            (0..256).fold(1i32, |h, _| h.wrapping_mul(31).wrapping_sub(1))
        );
        // An empty list hashes to 1; a zero byte is replaced by 1.
        assert_eq!(last_seen_checksum(&[]), 1);
    }

    #[test]
    fn tracker_window_and_offsets() {
        let mut t = Tracker::new();
        let (seen, update) = t.generate_update();
        assert!(seen.is_empty());
        assert_eq!(
            update,
            LastSeenUpdate {
                offset: 0,
                acknowledged: 0,
                checksum: 1
            }
        );

        assert!(t.add_pending(sig(1), true));
        assert!(
            !t.add_pending(sig(1), true),
            "the same message twice in a row counts once"
        );
        assert!(
            t.add_pending(sig(2), false),
            "not shown: takes a slot, tracks nothing"
        );
        assert!(t.add_pending(sig(3), true));
        let (seen, update) = t.generate_update();
        assert_eq!(seen, [sig(1), sig(3)]);
        assert_eq!(update.offset, 3);
        // The window is read from the tail: 17 empty slots, then ours.
        assert_eq!(update.acknowledged, 1 << 17 | 1 << 19);
        assert_eq!(update.checksum, last_seen_checksum(&[sig(1), sig(3)]));
        // Nothing new: offset back to zero, same messages.
        let (seen, update) = t.generate_update();
        assert_eq!((seen.len(), update.offset), (2, 0));

        // A deleted message that was still pending drops out; an
        // acknowledged one stays.
        t.add_pending(sig(4), true);
        t.ignore_pending(&sig(4));
        t.ignore_pending(&sig(1));
        let (seen, _) = t.generate_update();
        assert_eq!(seen, [sig(1), sig(3)]);
    }

    #[test]
    fn update_wire_format() {
        let mut out = Vec::new();
        LastSeenUpdate {
            offset: 3,
            acknowledged: 1 << 17 | 1 << 19,
            checksum: 7,
        }
        .encode(&mut out);
        assert_eq!(out, [3, 0, 0, 0b1010, 7]);
    }

    #[test]
    fn cache_order() {
        let mut c = SignatureCache::new();
        // A message with signature 3 that had seen 1 and 2.
        c.push(&[sig(1), sig(2)], Some(&sig(3)));
        assert_eq!(
            c.entries[..4],
            [Some(sig(3)), Some(sig(2)), Some(sig(1)), None]
        );
        // The next one saw 2 and 3: they move to the front, 1 shifts back.
        c.push(&[sig(2), sig(3)], Some(&sig(4)));
        assert_eq!(
            c.entries[..5],
            [Some(sig(4)), Some(sig(3)), Some(sig(2)), Some(sig(1)), None]
        );
        // Packed IDs are index + 1; 0 means a full signature follows.
        let mut packed: &[u8] = &[4];
        assert_eq!(c.read(&mut packed).unwrap(), Some(sig(1)));
        let mut full = vec![0u8];
        full.extend_from_slice(&sig(9));
        assert_eq!(c.read(&mut full.as_slice()).unwrap(), Some(sig(9)));
        assert_eq!(c.read(&mut [100u8].as_slice()).unwrap(), None);
    }

    fn player_chat(
        global_index: i32,
        index: i32,
        sender: Uuid,
        signature: Option<Signature>,
        text: &str,
        timestamp: i64,
        salt: i64,
    ) -> Vec<u8> {
        player_chat_with(
            global_index,
            index,
            sender,
            signature,
            text,
            timestamp,
            salt,
            None,
            (0, &[]),
        )
    }

    #[allow(clippy::too_many_arguments)]
    fn player_chat_with(
        global_index: i32,
        index: i32,
        sender: Uuid,
        signature: Option<Signature>,
        text: &str,
        timestamp: i64,
        salt: i64,
        unsigned: Option<rapidbot_nbt::Tag>,
        mask: (i32, &[u8]),
    ) -> Vec<u8> {
        let mut b = Vec::new();
        VarInt(global_index).encode(&mut b);
        sender.encode(&mut b);
        VarInt(index).encode(&mut b);
        signature.is_some().encode(&mut b);
        if let Some(s) = signature {
            b.extend_from_slice(&s);
        }
        text.encode(&mut b);
        timestamp.encode(&mut b);
        salt.encode(&mut b);
        VarInt(0).encode(&mut b); // last seen
        unsigned.is_some().encode(&mut b);
        if let Some(unsigned) = unsigned {
            unsigned.encode(&mut b);
        }
        VarInt(mask.0).encode(&mut b);
        if mask.0 == 2 {
            VarInt(mask.1.len() as i32).encode(&mut b);
            b.extend_from_slice(mask.1);
        }
        b
    }

    fn test_session() -> (SigningKey<Sha256>, ChatSession) {
        let key = rsa::RsaPrivateKey::new(&mut rand::thread_rng(), 2048).unwrap();
        let public = key.to_public_key();
        let public_der = rsa::pkcs8::EncodePublicKey::to_public_key_der(&public)
            .unwrap()
            .as_bytes()
            .to_vec();
        (
            SigningKey::new(key),
            ChatSession {
                session_id: Uuid::from_u128(3),
                expires_at: i64::MAX / 2,
                public_key_der: public_der,
            },
        )
    }

    fn signed(
        key: &SigningKey<Sha256>,
        session: &ChatSession,
        sender: Uuid,
        index: i32,
        text: &str,
        timestamp: i64,
        salt: i64,
    ) -> Signature {
        let data = signed_bytes(
            sender,
            session.session_id,
            index,
            text,
            timestamp,
            salt,
            &[],
        );
        let bytes = key.sign(&data).to_bytes();
        bytes.as_ref().try_into().unwrap()
    }

    fn info_with_session(session: &ChatSession) -> PlayerInfo {
        PlayerInfo {
            chat_session: Some(session.clone()),
            ..Default::default()
        }
    }

    #[test]
    fn incoming_chat_is_tracked_and_acknowledged() {
        let mut chat = ChatState::new();
        let sender = Uuid::from_u128(7);
        let info = PlayerInfo::default();
        let got = chat
            .player_chat(
                &player_chat(0, 0, sender, None, "hi", 0, 0),
                |_| Some(&info),
                false,
            )
            .unwrap();
        assert_eq!(
            got.incoming,
            Some(Incoming {
                sender,
                text: "hi".into()
            })
        );
        assert_eq!(got.ack, None);
        // Unsigned messages are not tracked.
        assert_eq!(chat.tracker.offset, 0);

        // The index must count up.
        assert!(matches!(
            chat.player_chat(
                &player_chat(5, 5, sender, None, "x", 0, 0),
                |_| Some(&info),
                false
            ),
            Err(ChatError::BadIndex {
                expected: 1,
                got: 5
            })
        ));

        // 65 signed messages: the 65th pushes the offset past 64.
        let (key, session) = test_session();
        let info = info_with_session(&session);
        let mut chat = ChatState::new();
        for i in 0..65i32 {
            let timestamp = 1_000 + i as i64;
            let salt = i as i64;
            let signature = signed(&key, &session, sender, i, "m", timestamp, salt);
            let got = chat
                .player_chat(
                    &player_chat(i, i, sender, Some(signature), "m", timestamp, salt),
                    |_| Some(&info),
                    false,
                )
                .unwrap();
            assert_eq!(got.ack, (i == 64).then_some(65), "message {i}");
            assert_eq!(got.incoming.unwrap().text, "m");
        }
        // Our next message reports the last 20 and no further offset.
        let Outgoing::Chat(sb::Chat {
            signature,
            last_seen,
            ..
        }) = chat.chat("hello", 1_000)
        else {
            panic!()
        };
        assert!(signature.is_none(), "no chat session: unsigned");
        assert_eq!(last_seen.offset, 0);
        assert_eq!(last_seen.acknowledged, (1 << 20) - 1);
    }

    #[test]
    fn sender_signatures_are_checked_and_broken_chains_stay_broken() {
        let (key, session) = test_session();
        let sender = Uuid::from_u128(7);
        let info = info_with_session(&session);
        let mut chat = ChatState::new();
        let timestamp = 1_000;
        let salt = 7;

        let first = signed(&key, &session, sender, 0, "hello", timestamp, salt);
        let got = chat
            .player_chat(
                &player_chat(0, 0, sender, Some(first), "hello", timestamp, salt),
                |_| Some(&info),
                false,
            )
            .unwrap();
        assert_eq!(got.incoming.unwrap().text, "hello");

        // The same signed message twice is accepted (the packet index
        // still has to advance).
        let got = chat
            .player_chat(
                &player_chat(1, 0, sender, Some(first), "hello", timestamp, salt),
                |_| Some(&info),
                false,
            )
            .unwrap();
        assert_eq!(got.incoming.unwrap().text, "hello");

        // A valid signature over different content is caught.
        let second = signed(&key, &session, sender, 1, "good", timestamp, salt + 1);
        let got = chat
            .player_chat(
                &player_chat(2, 1, sender, Some(second), "bad", timestamp, salt + 1),
                |_| Some(&info),
                false,
            )
            .unwrap();
        assert_eq!(got.incoming, None);

        // The chain is now broken for this session, even for a valid
        // signature at the right index.
        let third = signed(&key, &session, sender, 2, "next", timestamp, salt + 2);
        let got = chat
            .player_chat(
                &player_chat(3, 2, sender, Some(third), "next", timestamp, salt + 2),
                |_| Some(&info),
                false,
            )
            .unwrap();
        assert_eq!(got.incoming, None);

        // A new chat session starts a fresh chain.
        let new_session = ChatSession {
            session_id: Uuid::from_u128(4),
            ..session
        };
        let new_info = info_with_session(&new_session);
        let fresh = signed(&key, &new_session, sender, 0, "fresh", timestamp, salt);
        let got = chat
            .player_chat(
                &player_chat(4, 0, sender, Some(fresh), "fresh", timestamp, salt),
                |_| Some(&new_info),
                false,
            )
            .unwrap();
        assert_eq!(got.incoming.unwrap().text, "fresh");
    }

    #[test]
    fn missing_or_expired_profile_keys_follow_secure_chat_settings() {
        let (key, mut session) = test_session();
        let sender = Uuid::from_u128(7);
        let timestamp = 1_000;
        let salt = 7;
        let signature = signed(&key, &session, sender, 0, "hi", timestamp, salt);
        let body = player_chat(0, 0, sender, Some(signature), "hi", timestamp, salt);

        let no_session = PlayerInfo::default();
        let mut chat = ChatState::new();
        assert_eq!(
            chat.player_chat(&body, |_| Some(&no_session), true)
                .unwrap()
                .incoming,
            None,
            "a secure server rejects messages without a profile key"
        );
        let mut chat = ChatState::new();
        assert_eq!(
            chat.player_chat(&body, |_| Some(&no_session), false)
                .unwrap()
                .incoming
                .unwrap()
                .text,
            "hi",
            "an insecure server shows the content unsigned"
        );

        // Expiry has an eight-hour grace period.
        session.expires_at = crate::clock::millis() - PROFILE_KEY_GRACE_MS - 1;
        let expired = info_with_session(&session);
        let mut chat = ChatState::new();
        assert_eq!(
            chat.player_chat(&body, |_| Some(&expired), false)
                .unwrap()
                .incoming,
            None
        );
    }

    #[test]
    fn unknown_senders_are_not_shown() {
        let sender = Uuid::from_u128(7);
        let mut chat = ChatState::new();
        let got = chat
            .player_chat(
                &player_chat(0, 0, sender, Some(sig(1)), "hi", 0, 0),
                |_| None,
                false,
            )
            .unwrap();
        assert_eq!(got.incoming, None);
        // The bad message still occupies a last-seen slot as unshown.
        assert_eq!(chat.tracker.offset, 1);
    }

    #[test]
    fn commands_pick_their_packet() {
        let mut chat = ChatState::new();
        assert_eq!(
            chat.command("home", &[], 0),
            Outgoing::Command(sb::ChatCommand {
                command: "home".into()
            })
        );
        // A message argument without a session: the signed packet, with no
        // signatures in it.
        let signable = [("message".to_owned(), "hi".to_owned())];
        let Outgoing::SignedCommand(sb::ChatCommandSigned {
            command,
            signatures,
            ..
        }) = chat.command("msg Steve hi", &signable, 0)
        else {
            panic!()
        };
        assert_eq!(command, "msg Steve hi");
        assert!(signatures.is_empty());
    }

    #[test]
    fn signed_bytes_layout() {
        let data = signed_bytes(
            Uuid::from_u128(1),
            Uuid::from_u128(2),
            3,
            "hé",
            1_999,
            5,
            &[sig(9)],
        );
        let mut expected = vec![0, 0, 0, 1];
        expected.extend_from_slice(&1u128.to_be_bytes());
        expected.extend_from_slice(&2u128.to_be_bytes());
        expected.extend_from_slice(&[0, 0, 0, 3]);
        expected.extend_from_slice(&5i64.to_be_bytes());
        expected.extend_from_slice(&1i64.to_be_bytes()); // 1999 ms is second 1
        expected.extend_from_slice(&[0, 0, 0, 3]); // "hé" is three UTF-8 bytes
        expected.extend_from_slice("hé".as_bytes());
        expected.extend_from_slice(&[0, 0, 0, 1]);
        expected.extend_from_slice(&sig(9));
        assert_eq!(data, expected);
    }

    #[test]
    fn normalizing() {
        assert_eq!(normalize("  hello   world \u{a7}c!\n"), "hello world c!");
        assert_eq!(normalize(&"x".repeat(300)).len(), 256);
        assert_eq!(normalize("   "), "");
        // `Character.isWhitespace` does not treat non-breaking spaces as
        // whitespace; they survive untouched.
        assert_eq!(normalize("a\u{a0}b"), "a\u{a0}b");
        assert_eq!(normalize("a\u{202f}b"), "a\u{202f}b");
        // Unicode spaces are trimmed and collapsed.
        assert_eq!(normalize("\u{2003}a\u{2003}\u{3000}b\u{2003}"), "a b");
        // A surrogate pair cut at 256 UTF-16 units becomes `?`.
        let mut long = "x".repeat(255);
        long.push('\u{1f600}');
        assert_eq!(normalize(&long), format!("{}?", "x".repeat(255)));
    }

    #[test]
    fn unsigned_content_and_filter_masks_shape_display() {
        let mut chat = ChatState::new();
        let sender = Uuid::from_u128(7);
        let info = PlayerInfo::default();

        // Pass-through mask: the unsigned content is shown.
        let body = player_chat_with(
            0,
            0,
            sender,
            None,
            "signed",
            0,
            0,
            Some(rapidbot_nbt::Tag::String("shown".into())),
            (0, &[]),
        );
        let got = chat.player_chat(&body, |_| Some(&info), false).unwrap();
        assert_eq!(got.incoming.unwrap().text, "shown");

        // A partial mask masks the signed content, even when unsigned
        // content is present (`applyWithFormatting(signedContent)`).
        let body = player_chat_with(
            1,
            1,
            sender,
            None,
            "abcd",
            0,
            0,
            Some(rapidbot_nbt::Tag::String("ignored".into())),
            (2, &[0b0101]),
        );
        let got = chat.player_chat(&body, |_| Some(&info), false).unwrap();
        assert_eq!(got.incoming.unwrap().text, "#b#d");

        // Fully filtered messages are not shown but still counted.
        let body = player_chat_with(2, 2, sender, None, "secret", 0, 0, None, (1, &[]));
        let got = chat.player_chat(&body, |_| Some(&info), false).unwrap();
        assert!(got.incoming.is_none());

        // More than 20 last-seen entries is a protocol violation.
        let mut body = Vec::new();
        VarInt(3).encode(&mut body); // global index
        sender.encode(&mut body);
        VarInt(3).encode(&mut body); // index
        false.encode(&mut body); // no signature
        "x".encode(&mut body);
        0i64.encode(&mut body); // timestamp
        0i64.encode(&mut body); // salt
        VarInt(21).encode(&mut body); // 21 last-seen entries
        assert!(chat.player_chat(&body, |_| Some(&info), false).is_err());
    }
}
