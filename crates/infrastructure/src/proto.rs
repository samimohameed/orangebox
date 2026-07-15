//! Minimal protobuf wire-format walker.
//!
//! Blackbox does not ship any vendor's `.proto` schemas. Instead this module
//! decodes the wire format generically (field number + wire type + payload)
//! and adapters pick out the fields they have mapped by observation
//! (documented in FORMATS.md). Unknown fields are preserved in the walk
//! result, so a format change degrades to "fields we don't understand yet",
//! never a crash.

/// One decoded field occurrence.
#[derive(Debug, Clone, PartialEq)]
pub enum Value {
    Varint(u64),
    /// Length-delimited payload that parses as valid UTF-8.
    Str(String),
    /// Length-delimited payload that does not parse as UTF-8 (may be a
    /// nested message — call `walk` on it again).
    Bytes(Vec<u8>),
    Fixed32(u32),
    Fixed64(u64),
}

#[derive(Debug, Clone, PartialEq)]
pub struct Field {
    pub number: u64,
    pub value: Value,
}

fn read_varint(buf: &[u8], i: &mut usize) -> Option<u64> {
    let mut value: u64 = 0;
    let mut shift = 0u32;
    loop {
        let byte = *buf.get(*i)?;
        *i += 1;
        value |= u64::from(byte & 0x7f) << shift;
        if byte & 0x80 == 0 {
            return Some(value);
        }
        shift += 7;
        if shift >= 64 {
            return None;
        }
    }
}

/// Decode one message level. Returns `None` if the buffer is not valid
/// wire format (callers use this to distinguish nested messages from
/// plain byte strings).
pub fn walk(buf: &[u8]) -> Option<Vec<Field>> {
    let mut fields = Vec::new();
    let mut i = 0usize;
    while i < buf.len() {
        let tag = read_varint(buf, &mut i)?;
        let number = tag >> 3;
        if number == 0 {
            return None;
        }
        let value = match tag & 0x7 {
            0 => Value::Varint(read_varint(buf, &mut i)?),
            1 => {
                let bytes = buf.get(i..i + 8)?;
                i += 8;
                Value::Fixed64(u64::from_le_bytes(bytes.try_into().ok()?))
            }
            2 => {
                let len = read_varint(buf, &mut i)? as usize;
                let payload = buf.get(i..i + len)?;
                i += len;
                match std::str::from_utf8(payload) {
                    Ok(s) => Value::Str(s.to_string()),
                    Err(_) => Value::Bytes(payload.to_vec()),
                }
            }
            5 => {
                let bytes = buf.get(i..i + 4)?;
                i += 4;
                Value::Fixed32(u32::from_le_bytes(bytes.try_into().ok()?))
            }
            _ => return None,
        };
        fields.push(Field { number, value });
    }
    Some(fields)
}

/// First occurrence of `number` in `fields`.
pub fn first<'a>(fields: &'a [Field], number: u64) -> Option<&'a Value> {
    fields.iter().find(|f| f.number == number).map(|f| &f.value)
}

/// First occurrence of `number`, decoded as a nested message.
pub fn first_message(fields: &[Field], number: u64) -> Option<Vec<Field>> {
    match first(fields, number)? {
        Value::Bytes(b) => walk(b),
        // A nested message whose payload happens to be valid UTF-8 still
        // decodes as Str; re-walk its bytes.
        Value::Str(s) => walk(s.as_bytes()),
        _ => None,
    }
}

pub fn as_str(value: &Value) -> Option<&str> {
    match value {
        Value::Str(s) => Some(s),
        _ => None,
    }
}

pub fn as_varint(value: &Value) -> Option<u64> {
    match value {
        Value::Varint(v) => Some(*v),
        _ => None,
    }
}

/// Decode a `{1: seconds, 2: nanos}` timestamp message into epoch millis.
pub fn timestamp_ms(fields: &[Field], number: u64) -> Option<i64> {
    let ts = first_message(fields, number)?;
    let seconds = as_varint(first(&ts, 1)?)? as i64;
    let nanos = first(&ts, 2).and_then(as_varint).unwrap_or(0) as i64;
    Some(seconds * 1000 + nanos / 1_000_000)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Hand-encode: field 1 = "hello", field 2 = varint 42,
    /// field 3 = nested {1: varint 7}.
    fn sample() -> Vec<u8> {
        let mut b = vec![];
        b.extend([0x0a, 5]);
        b.extend(b"hello");
        b.extend([0x10, 42]);
        b.extend([0x1a, 2, 0x08, 7]);
        b
    }

    #[test]
    fn walks_strings_varints_and_nested_messages() {
        let fields = walk(&sample()).unwrap();
        assert_eq!(as_str(first(&fields, 1).unwrap()), Some("hello"));
        assert_eq!(as_varint(first(&fields, 2).unwrap()), Some(42));
        let nested = first_message(&fields, 3).unwrap();
        assert_eq!(as_varint(first(&nested, 1).unwrap()), Some(7));
    }

    #[test]
    fn garbage_returns_none_not_panic() {
        assert_eq!(walk(&[0xff, 0xff, 0xff]), None);
        // Field number 0 is invalid wire format.
        assert_eq!(walk(&[0x02, 0x01]), None);
    }

    #[test]
    fn timestamp_decodes_to_millis() {
        // {3: {1: 1779579752, 2: 605286000}}
        let mut b = vec![0x1a];
        let mut inner = vec![0x08];
        inner.extend(encode_varint(1_779_579_752));
        inner.push(0x10);
        inner.extend(encode_varint(605_286_000));
        b.push(inner.len() as u8);
        b.extend(&inner);
        let fields = walk(&b).unwrap();
        assert_eq!(timestamp_ms(&fields, 3), Some(1_779_579_752_605));
    }

    fn encode_varint(mut v: u64) -> Vec<u8> {
        let mut out = vec![];
        loop {
            let byte = (v & 0x7f) as u8;
            v >>= 7;
            if v == 0 {
                out.push(byte);
                break;
            }
            out.push(byte | 0x80);
        }
        out
    }
}
