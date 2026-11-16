//! Message hashes as Claude Code writes them in `api-request.tail`.
//!
//! A hash is the SHA-256 of the message `{role, content}` serialized as
//! compact JSON with object keys sorted. An assistant message is the merge of
//! every content block logged under one `message.id`. Verified on real
//! transcripts: every `api-request-blob` hashes to its own `hash`, and most
//! tail entries hash to a transcript `user` record or a merged assistant
//! message.

use serde_json::Value;
use sha2::{Digest, Sha256};

pub type Hash = [u8; 32];

/// Compact JSON with sorted object keys, independent of `serde_json`'s
/// `preserve_order` feature (the app build enables it).
fn write(v: &Value, out: &mut Vec<u8>) {
    match v {
        Value::Object(m) => {
            let mut keys: Vec<&String> = m.keys().collect();
            keys.sort();
            out.push(b'{');
            for (i, k) in keys.into_iter().enumerate() {
                if i > 0 {
                    out.push(b',');
                }
                // Writing a string into a Vec cannot fail.
                let _ = serde_json::to_writer(&mut *out, k);
                out.push(b':');
                write(&m[k], out);
            }
            out.push(b'}');
        }
        Value::Array(a) => {
            out.push(b'[');
            for (i, x) in a.iter().enumerate() {
                if i > 0 {
                    out.push(b',');
                }
                write(x, out);
            }
            out.push(b']');
        }
        other => {
            let _ = serde_json::to_writer(&mut *out, other);
        }
    }
}

/// Hash of a message with this role and content.
pub fn hash(role: &str, content: &Value) -> Hash {
    let mut buf = Vec::with_capacity(1024);
    buf.extend_from_slice(br#"{"content":"#);
    write(content, &mut buf);
    buf.extend_from_slice(br#","role":"#);
    let _ = serde_json::to_writer(&mut buf, role);
    buf.push(b'}');
    Sha256::digest(&buf).into()
}

pub fn parse_hex(s: &str) -> Option<Hash> {
    let b = s.as_bytes();
    if b.len() != 64 {
        return None;
    }
    let mut h = [0u8; 32];
    for (i, out) in h.iter_mut().enumerate() {
        let hi = (b[2 * i] as char).to_digit(16)?;
        let lo = (b[2 * i + 1] as char).to_digit(16)?;
        *out = (hi * 16 + lo) as u8;
    }
    Some(h)
}

pub fn to_hex(h: &Hash) -> String {
    h.iter().map(|b| format!("{b:02x}")).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sorted_compact() {
        let v: Value = serde_json::from_str(r#"{"b":1,"a":[{"z":"é\n","y":null}]}"#).unwrap();
        let mut out = Vec::new();
        write(&v, &mut out);
        assert_eq!(
            String::from_utf8(out).unwrap(),
            r#"{"a":[{"y":null,"z":"é\n"}],"b":1}"#
        );
        let h = hash("user", &Value::String("hi".into()));
        assert_eq!(parse_hex(&to_hex(&h)), Some(h));
        // sha256 of {"content":"hi","role":"user"}
        assert_eq!(
            to_hex(&h),
            to_hex(&Sha256::digest(br#"{"content":"hi","role":"user"}"#).into())
        );
    }
}
