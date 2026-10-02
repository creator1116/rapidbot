//! Plain-text rendering of chat components, for logs and error messages.

use rapidbot_nbt::Tag;
use serde_json::Value;

/// A component sent as NBT (configuration and play).
pub fn nbt_to_plain(tag: &Tag) -> String {
    let mut out = String::new();
    nbt_append(tag, &mut out);
    out
}

fn nbt_append(tag: &Tag, out: &mut String) {
    match tag {
        Tag::String(s) => out.push_str(s),
        Tag::List(items) => items.iter().for_each(|t| nbt_append(t, out)),
        Tag::Compound(c) => {
            if let Some(text) = c.get("text").and_then(Tag::as_str) {
                out.push_str(text);
            } else if let Some(key) = c.get("translate").and_then(Tag::as_str) {
                out.push_str(key);
                if let Some(args) = c.get("with").and_then(Tag::as_list) {
                    out.push('[');
                    for (i, arg) in args.iter().enumerate() {
                        if i > 0 {
                            out.push_str(", ");
                        }
                        nbt_append(arg, out);
                    }
                    out.push(']');
                }
            }
            if let Some(extra) = c.get("extra").and_then(Tag::as_list) {
                extra.iter().for_each(|t| nbt_append(t, out));
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
    match serde_json::from_str::<Value>(json) {
        Ok(v) => {
            let mut out = String::new();
            json_append(&v, &mut out);
            out
        }
        Err(_) => json.to_owned(),
    }
}

fn json_append(v: &Value, out: &mut String) {
    match v {
        Value::String(s) => out.push_str(s),
        Value::Array(items) => items.iter().for_each(|t| json_append(t, out)),
        Value::Object(o) => {
            if let Some(Value::String(text)) = o.get("text") {
                out.push_str(text);
            } else if let Some(Value::String(key)) = o.get("translate") {
                out.push_str(key);
            }
            if let Some(Value::Array(extra)) = o.get("extra") {
                extra.iter().for_each(|t| json_append(t, out));
            }
        }
        other => out.push_str(&other.to_string()),
    }
}
