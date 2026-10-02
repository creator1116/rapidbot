//! The server's command tree (`ClientboundCommandsPacket`).
//!
//! The client needs it to send a command the way vanilla does: a command
//! with `message` arguments (`/msg`, `/me`, `/say`, ...) goes out as
//! `chat_command_signed` with those arguments signed, anything else as a
//! plain `chat_command` (`ClientPacketListener.sendCommand`). Which one
//! applies is decided by parsing the command against the tree, as
//! brigadier does.

use rapidbot_buf::{Decode, DecodeError, VarInt};
use rapidbot_world::Registry;

/// How much input an argument takes. Brigadier asks each argument type;
/// only the shape matters here, not the value.
#[derive(Debug, Clone, Copy, PartialEq)]
enum Parser {
    /// `StringArgumentType` single word.
    Word,
    /// `StringArgumentType` quotable phrase.
    Quotable,
    /// `StringArgumentType` greedy phrase: the rest of the line.
    Greedy,
    /// `MessageArgument`: the rest of the line, and signed.
    Message,
    /// Coordinates: this many space-separated parts.
    Parts(u8),
    /// Anything else: up to the next space outside brackets and quotes
    /// (selectors, NBT, components and block states may contain spaces).
    Token,
}

#[derive(Debug, Clone)]
enum Kind {
    Root,
    Literal(String),
    Argument { name: String, parser: Parser },
}

#[derive(Debug, Clone)]
struct Node {
    kind: Kind,
    children: Vec<usize>,
    redirect: Option<usize>,
}

#[derive(Debug, Clone, Default)]
pub struct CommandTree {
    nodes: Vec<Node>,
    root: usize,
}

/// `ArgumentSignatures.MAX_ARGUMENT_COUNT`.
const MAX_SIGNED_ARGUMENTS: usize = 8;

impl CommandTree {
    /// Decodes the body of a `commands` packet.
    pub fn decode(mut body: &[u8]) -> Result<Self, DecodeError> {
        let buf = &mut body;
        let count = VarInt::decode(buf)?.0;
        let mut nodes = Vec::with_capacity(count.max(0) as usize);
        for _ in 0..count {
            let flags = u8::decode(buf)?;
            let children = Vec::<VarInt>::decode(buf)?.into_iter().map(|v| v.0 as usize).collect();
            let redirect = if flags & 8 != 0 { Some(VarInt::decode(buf)?.0 as usize) } else { None };
            let kind = match flags & 3 {
                2 => {
                    let name = String::decode(buf)?;
                    let parser = read_parser(buf)?;
                    if flags & 16 != 0 {
                        // Suggestion provider.
                        String::decode(buf)?;
                    }
                    Kind::Argument { name, parser }
                }
                1 => Kind::Literal(String::decode(buf)?),
                _ => Kind::Root,
            };
            nodes.push(Node { kind, children, redirect });
        }
        let root = VarInt::decode(buf)?.0 as usize;
        Ok(Self { nodes, root })
    }

    pub fn is_empty(&self) -> bool {
        self.nodes.is_empty()
    }

    /// `SignableCommand.of(parse(command)).arguments()`: name and text of
    /// each `message` argument, in order. Empty for a command the tree does
    /// not know.
    pub fn signable_arguments(&self, command: &str) -> Vec<(String, String)> {
        if self.nodes.is_empty() {
            return Vec::new();
        }
        let (_, mut arguments) = self.parse(self.root, command, 0, true, 0);
        arguments.truncate(MAX_SIGNED_ARGUMENTS);
        arguments
    }

    /// `CommandDispatcher.parseNodes`: the parse that gets furthest, as
    /// (position reached, signable arguments on the way).
    fn parse(&self, node: usize, input: &str, start: usize, signing: bool, depth: usize) -> (usize, Vec<(String, String)>) {
        let mut best = (start, Vec::new());
        if depth > 64 {
            return best;
        }
        for &child in self.relevant_children(node, input, start) {
            let Some(end) = self.consume(child, input, start) else { continue };
            // Brigadier: an argument must be followed by a space or the end.
            if end < input.len() && input.as_bytes()[end] != b' ' {
                continue;
            }
            let mut arguments = Vec::new();
            if let Kind::Argument { name, parser: Parser::Message } = &self.nodes[child].kind {
                if signing {
                    arguments.push((name.clone(), input[start..end].to_owned()));
                }
            }
            let mut reached = end;
            let redirect = self.nodes[child].redirect;
            // `reader.canRead(redirect == null ? 2 : 1)`
            if input.len() - end >= if redirect.is_none() { 2 } else { 1 } {
                let (next, signing) = match redirect {
                    // Arguments behind a redirect to the root (`execute run`)
                    // are not signed (`rejectRootRedirects`).
                    Some(target) => (target, signing && target != self.root),
                    None => (child, signing),
                };
                let (end, more) = self.parse(next, input, end + 1, signing, depth + 1);
                reached = end;
                arguments.extend(more);
            }
            if reached > best.0 || (reached == best.0 && arguments.len() > best.1.len()) {
                best = (reached, arguments);
            }
        }
        best
    }

    /// `CommandNode.getRelevantNodes`: the literal named by the next word
    /// if there is one, else every argument.
    fn relevant_children(&self, node: usize, input: &str, start: usize) -> impl Iterator<Item = &usize> {
        let children = &self.nodes[node].children;
        let word = input[start..].split(' ').next().unwrap_or("");
        let literal = children
            .iter()
            .position(|&c| matches!(&self.nodes.get(c).map(|n| &n.kind), Some(Kind::Literal(name)) if name == word));
        children.iter().enumerate().filter_map(move |(i, c)| match literal {
            Some(l) => (i == l).then_some(c),
            None => matches!(self.nodes.get(*c).map(|n| &n.kind), Some(Kind::Argument { .. })).then_some(c),
        })
    }

    /// Where `child` stops reading, or `None` if it does not match.
    fn consume(&self, child: usize, input: &str, start: usize) -> Option<usize> {
        let rest = &input[start..];
        let length = match &self.nodes[child].kind {
            Kind::Root => return None,
            Kind::Literal(name) => rest.starts_with(name.as_str()).then_some(name.len())?,
            Kind::Argument { parser, .. } => match parser {
                Parser::Greedy | Parser::Message => rest.len(),
                Parser::Word => rest.find(|c: char| !is_unquoted(c)).unwrap_or(rest.len()),
                Parser::Quotable => match rest.chars().next() {
                    Some(quote @ ('"' | '\'')) => quoted_length(rest, quote)?,
                    _ => rest.find(|c: char| !is_unquoted(c)).unwrap_or(rest.len()),
                },
                Parser::Parts(n) => {
                    let mut end = 0;
                    for i in 0..*n {
                        if i > 0 {
                            if rest.as_bytes().get(end) != Some(&b' ') {
                                return None;
                            }
                            end += 1;
                        }
                        let part = rest[end..].find(' ').unwrap_or(rest.len() - end);
                        if part == 0 {
                            return None;
                        }
                        end += part;
                    }
                    end
                }
                Parser::Token => token_length(rest),
            },
        };
        (length > 0).then_some(start + length)
    }
}

/// `StringReader.isAllowedInUnquotedString`.
fn is_unquoted(c: char) -> bool {
    c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '.' | '+')
}

/// Length of a quoted string including both quotes.
fn quoted_length(s: &str, quote: char) -> Option<usize> {
    let mut escaped = false;
    for (i, c) in s.char_indices().skip(1) {
        if escaped {
            escaped = false;
        } else if c == '\\' {
            escaped = true;
        } else if c == quote {
            return Some(i + c.len_utf8());
        }
    }
    None
}

/// Up to the next space that is not inside brackets or quotes.
fn token_length(s: &str) -> usize {
    let mut depth = 0usize;
    let mut quote: Option<char> = None;
    let mut escaped = false;
    for (i, c) in s.char_indices() {
        if let Some(q) = quote {
            if escaped {
                escaped = false;
            } else if c == '\\' {
                escaped = true;
            } else if c == q {
                quote = None;
            }
            continue;
        }
        match c {
            '"' | '\'' => quote = Some(c),
            '[' | '{' => depth += 1,
            ']' | '}' => depth = depth.saturating_sub(1),
            ' ' if depth == 0 => return i,
            _ => {}
        }
    }
    s.len()
}

/// An argument node's parser: its ID in `minecraft:command_argument_type`,
/// then whatever that type's `serializeToNetwork` wrote.
fn read_parser(buf: &mut &[u8]) -> Result<Parser, DecodeError> {
    let id = VarInt::decode(buf)?.0;
    let name = Registry::get().command_argument_types.get(id as usize).map(String::as_str).unwrap_or("");
    let number = |buf: &mut &[u8], size: usize| -> Result<(), DecodeError> {
        // ArgumentUtils.createNumberFlags: bit 0 minimum, bit 1 maximum.
        let flags = u8::decode(buf)?;
        rapidbot_buf::take(buf, size * (flags & 1) as usize + size * (flags >> 1 & 1) as usize)?;
        Ok(())
    };
    Ok(match name {
        "brigadier:float" | "brigadier:integer" => {
            number(buf, 4)?;
            Parser::Token
        }
        "brigadier:double" | "brigadier:long" => {
            number(buf, 8)?;
            Parser::Token
        }
        "brigadier:string" => match VarInt::decode(buf)?.0 {
            0 => Parser::Word,
            1 => Parser::Quotable,
            _ => Parser::Greedy,
        },
        "minecraft:entity" | "minecraft:score_holder" => {
            u8::decode(buf)?;
            Parser::Token
        }
        "minecraft:time" => {
            i32::decode(buf)?;
            Parser::Token
        }
        "minecraft:resource_or_tag"
        | "minecraft:resource_or_tag_key"
        | "minecraft:resource"
        | "minecraft:resource_key"
        | "minecraft:resource_selector" => {
            String::decode(buf)?;
            Parser::Token
        }
        "minecraft:message" => Parser::Message,
        "minecraft:block_pos" | "minecraft:vec3" => Parser::Parts(3),
        "minecraft:column_pos" | "minecraft:vec2" | "minecraft:rotation" => Parser::Parts(2),
        _ => Parser::Token,
    })
}

#[cfg(test)]
mod tests {
    use rapidbot_buf::Encode;

    use super::*;

    fn parser_id(name: &str) -> i32 {
        Registry::get().command_argument_types.iter().position(|n| n == name).unwrap() as i32
    }

    fn node(out: &mut Vec<u8>, flags: u8, children: &[i32], redirect: Option<i32>, name: Option<&str>, parser: Option<(&str, &[u8])>) {
        out.push(flags | if redirect.is_some() { 8 } else { 0 });
        VarInt(children.len() as i32).encode(out);
        for c in children {
            VarInt(*c).encode(out);
        }
        if let Some(r) = redirect {
            VarInt(r).encode(out);
        }
        if let Some(name) = name {
            name.encode(out);
        }
        if let Some((parser, properties)) = parser {
            VarInt(parser_id(parser)).encode(out);
            out.extend_from_slice(properties);
        }
    }

    /// root ─ msg ─ targets(entity) ─ message(message)
    ///      ├ tell → msg
    ///      ├ tp ─ location(vec3)
    ///      ├ give ─ target(entity) ─ count(integer 1..)
    ///      └ execute ─ run → root
    fn tree() -> CommandTree {
        let mut b = Vec::new();
        VarInt(11).encode(&mut b);
        node(&mut b, 0, &[1, 4, 5, 7, 9], None, None, None); // 0 root
        node(&mut b, 1, &[2], None, Some("msg"), None); // 1
        node(&mut b, 2, &[3], None, Some("targets"), Some(("minecraft:entity", &[3]))); // 2
        node(&mut b, 2 | 4, &[], None, Some("message"), Some(("minecraft:message", &[]))); // 3
        node(&mut b, 1, &[], Some(1), Some("tell"), None); // 4
        node(&mut b, 1, &[6], None, Some("tp"), None); // 5
        node(&mut b, 2 | 4, &[], None, Some("location"), Some(("minecraft:vec3", &[]))); // 6
        node(&mut b, 1, &[8], None, Some("give"), None); // 7
        node(&mut b, 2, &[10], None, Some("target"), Some(("minecraft:entity", &[2]))); // 8
        node(&mut b, 1, &[], Some(0), Some("execute"), None); // 9 (stands in for `execute run`)
        node(&mut b, 2 | 4, &[], None, Some("count"), Some(("brigadier:integer", &[1, 0, 0, 0, 1]))); // 10
        VarInt(0).encode(&mut b);
        CommandTree::decode(&b).unwrap()
    }

    #[test]
    fn message_arguments_are_signable() {
        let t = tree();
        assert_eq!(t.signable_arguments("msg Steve hello there  friend"), [("message".to_owned(), "hello there  friend".to_owned())]);
        // Through an alias (a redirect to another node).
        assert_eq!(t.signable_arguments("tell Steve hi"), [("message".to_owned(), "hi".to_owned())]);
        // A selector with spaces is one argument.
        assert_eq!(
            t.signable_arguments("msg @a[name=\"Old Steve\", limit=1] hi all"),
            [("message".to_owned(), "hi all".to_owned())]
        );
    }

    #[test]
    fn other_commands_are_not() {
        let t = tree();
        assert!(t.signable_arguments("tp 1 64 -3").is_empty());
        assert!(t.signable_arguments("give Steve 5").is_empty());
        assert!(t.signable_arguments("msg").is_empty());
        assert!(t.signable_arguments("msg Steve").is_empty());
        assert!(t.signable_arguments("unknown thing").is_empty());
        assert!(CommandTree::default().signable_arguments("msg Steve hi").is_empty());
        // Behind a redirect to the root nothing is signed.
        assert!(t.signable_arguments("execute msg Steve hi").is_empty());
    }
}
