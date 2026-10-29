//! Plain-text rendering of chat components, for logs and error messages.

use std::collections::HashMap;

use rapidbot_nbt::Tag;
use serde_json::Value;

/// A component sent as NBT (configuration and play).
pub fn nbt_to_plain(tag: &Tag) -> String {
    nbt_to_plain_translated(tag, &HashMap::new())
}

/// NBT like `nbt_to_plain`, with the active resource pack's language map.
pub fn nbt_to_plain_translated(tag: &Tag, translations: &HashMap<String, String>) -> String {
    let mut out = String::new();
    nbt_append(tag, &mut out, translations);
    out
}

fn nbt_append(tag: &Tag, out: &mut String, translations: &HashMap<String, String>) {
    match tag {
        Tag::String(s) => out.push_str(s),
        Tag::List(items) => items.iter().for_each(|t| nbt_append(t, out, translations)),
        Tag::Compound(c) => {
            if let Some(text) = c.get("text").and_then(Tag::as_str) {
                out.push_str(text);
            } else if let Some(key) = c.get("translate").and_then(Tag::as_str) {
                let args: Vec<String> = c
                    .get("with")
                    .and_then(Tag::as_list)
                    .map(|args| {
                        args.iter()
                            .map(|arg| {
                                let mut text = String::new();
                                nbt_append(arg, &mut text, translations);
                                text
                            })
                            .collect()
                    })
                    .unwrap_or_default();
                append_translation(key, &args, out, translations);
            }
            if let Some(extra) = c.get("extra").and_then(Tag::as_list) {
                extra.iter().for_each(|t| nbt_append(t, out, translations));
            }
        }
        other => {
            if let Some(n) = other.as_i64() {
                out.push_str(&n.to_string());
            }
        }
    }
}

/// A component sent as JSON (login disconnect).
pub fn json_to_plain(json: &str) -> String {
    json_to_plain_translated(json, &HashMap::new())
}

/// JSON like `json_to_plain`, with the active resource pack's language map.
pub fn json_to_plain_translated(json: &str, translations: &HashMap<String, String>) -> String {
    match serde_json::from_str::<Value>(json) {
        Ok(v) => {
            let mut out = String::new();
            json_append(&v, &mut out, translations);
            out
        }
        Err(_) => json.to_owned(),
    }
}

fn json_append(v: &Value, out: &mut String, translations: &HashMap<String, String>) {
    match v {
        Value::String(s) => out.push_str(s),
        Value::Array(items) => items.iter().for_each(|t| json_append(t, out, translations)),
        Value::Object(o) => {
            if let Some(Value::String(text)) = o.get("text") {
                out.push_str(text);
            } else if let Some(Value::String(key)) = o.get("translate") {
                let args: Vec<String> = o
                    .get("with")
                    .and_then(Value::as_array)
                    .map(|args| {
                        args.iter()
                            .map(|arg| {
                                let mut text = String::new();
                                json_append(arg, &mut text, translations);
                                text
                            })
                            .collect()
                    })
                    .unwrap_or_default();
                append_translation(key, &args, out, translations);
            }
            if let Some(Value::Array(extra)) = o.get("extra") {
                extra.iter().for_each(|t| json_append(t, out, translations));
            }
        }
        other => out.push_str(&other.to_string()),
    }
}

/// Use the pack's text when it has one, substituting Java's `%s`/`%d` and
/// `%n$s` placeholders. An unknown key keeps the old `key[arg, ...]` form so
/// logs still show what the server sent.
fn append_translation(
    key: &str,
    args: &[String],
    out: &mut String,
    translations: &HashMap<String, String>,
) {
    let Some(text) = translations.get(key) else {
        out.push_str(key);
        if !args.is_empty() {
            out.push('[');
            for (i, arg) in args.iter().enumerate() {
                if i > 0 {
                    out.push_str(", ");
                }
                out.push_str(arg);
            }
            out.push(']');
        }
        return;
    };
    let mut chars = text.chars().peekable();
    let mut next_arg = 0;
    while let Some(c) = chars.next() {
        if c != '%' {
            out.push(c);
            continue;
        }
        match chars.peek().copied() {
            Some('%') => {
                chars.next();
                out.push('%');
            }
            Some('s' | 'd') => {
                let spec = chars.next().unwrap_or('s');
                push_arg(args, next_arg, out, spec);
                next_arg += 1;
            }
            Some(c) if c.is_ascii_digit() => {
                let mut index = String::new();
                while let Some(c) = chars.peek().copied() {
                    if !c.is_ascii_digit() {
                        break;
                    }
                    index.push(c);
                    chars.next();
                }
                if chars.peek() == Some(&'$') {
                    chars.next();
                    let index = index
                        .parse::<usize>()
                        .ok()
                        .and_then(|n| n.checked_sub(1))
                        .unwrap_or(usize::MAX);
                    let spec = chars.next().unwrap_or('s');
                    push_arg(args, index, out, spec);
                } else {
                    out.push('%');
                    out.push_str(&index);
                }
            }
            _ => out.push('%'),
        }
    }
}

fn push_arg(args: &[String], index: usize, out: &mut String, spec: char) {
    match args.get(index) {
        Some(arg) => out.push_str(arg),
        None if spec == 'd' => out.push('0'),
        None => out.push_str("%s"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rapidbot_nbt::Compound;

    fn translations(entries: &[(&str, &str)]) -> HashMap<String, String> {
        entries
            .iter()
            .map(|(key, value)| ((*key).to_owned(), (*value).to_owned()))
            .collect()
    }

    fn translatable(key: &str, args: Vec<Tag>) -> Tag {
        let mut compound = Compound::default();
        compound.insert("translate", Tag::String(key.into()));
        if !args.is_empty() {
            compound.insert("with", Tag::List(args));
        }
        Tag::Compound(compound)
    }

    #[test]
    fn nbt_translations_substitute_arguments() {
        let map = translations(&[
            ("chat.joined", "%s joined the game"),
            ("chat.count", "%2$s has %1$s apples"),
        ]);
        assert_eq!(
            nbt_to_plain_translated(
                &translatable("chat.joined", vec![Tag::String("Steve".into())]),
                &map
            ),
            "Steve joined the game"
        );
        assert_eq!(
            nbt_to_plain_translated(
                &translatable("chat.count", vec![Tag::Int(3), Tag::String("Alex".into())]),
                &map
            ),
            "Alex has 3 apples"
        );
        // Unknown keys keep the old diagnostic form.
        assert_eq!(
            nbt_to_plain_translated(
                &translatable("chat.unknown", vec![Tag::String("x".into())]),
                &map
            ),
            "chat.unknown[x]"
        );
    }

    #[test]
    fn json_translations_substitute_arguments() {
        let map = translations(&[("chat.joined", "%s joined the game")]);
        assert_eq!(
            json_to_plain_translated(
                r#"{"translate":"chat.joined","with":["Steve"],"color":"yellow"}"#,
                &map
            ),
            "Steve joined the game"
        );
        assert_eq!(
            json_to_plain_translated(r#"{"text":"plain","extra":["!"]}"#, &map),
            "plain!"
        );
    }
}
