//! Minimal AMF0 encoder and decoder.
//!
//! Only the subset used by FLV script tags (`onMetaData`) and RTMP command objects is
//! implemented: numbers, booleans, strings, objects, ECMA arrays, strict arrays, null
//! and undefined. Property order is preserved so output is deterministic.
//!
//! The decoder is meant for untrusted input: nesting depth and the number of nodes are
//! bounded (see [`Limits`]) and nothing is pre-allocated from on-the-wire counts.

use std::fmt;

/// Default nesting limit: `onMetaData` is flat, command objects are two levels deep.
pub const MAX_DEPTH: usize = 16;
/// Default node budget: a value may consist of at most this many values and properties.
pub const MAX_NODES: usize = 4096;

/// Decoder limits.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Limits {
    /// Maximum nesting depth of objects and arrays.
    pub max_depth: usize,
    /// Maximum number of values (every scalar, container and property counts as one).
    pub max_nodes: usize,
}

impl Default for Limits {
    fn default() -> Self {
        Limits {
            max_depth: MAX_DEPTH,
            max_nodes: MAX_NODES,
        }
    }
}

/// An AMF0 value.
#[derive(Debug, Clone, PartialEq)]
pub enum Value {
    Number(f64),
    Boolean(bool),
    String(String),
    /// Anonymous object: ordered `(name, value)` properties.
    Object(Vec<(String, Value)>),
    Null,
    Undefined,
    /// Associative array; the on-wire count is a hint and is ignored when decoding.
    EcmaArray(Vec<(String, Value)>),
    StrictArray(Vec<Value>),
}

impl Value {
    /// Looks a property up by name in an object or ECMA array.
    pub fn get(&self, name: &str) -> Option<&Value> {
        match self {
            Value::Object(props) | Value::EcmaArray(props) => {
                props.iter().find(|(n, _)| n == name).map(|(_, v)| v)
            }
            _ => None,
        }
    }

    /// The numeric value, if this is a number.
    pub fn as_f64(&self) -> Option<f64> {
        match self {
            Value::Number(n) => Some(*n),
            _ => None,
        }
    }

    /// The boolean value, if this is a boolean.
    pub fn as_bool(&self) -> Option<bool> {
        match self {
            Value::Boolean(b) => Some(*b),
            _ => None,
        }
    }

    /// The string value, if this is a string.
    pub fn as_str(&self) -> Option<&str> {
        match self {
            Value::String(s) => Some(s),
            _ => None,
        }
    }
}

const MARKER_NUMBER: u8 = 0x00;
const MARKER_BOOLEAN: u8 = 0x01;
const MARKER_STRING: u8 = 0x02;
const MARKER_OBJECT: u8 = 0x03;
const MARKER_NULL: u8 = 0x05;
const MARKER_UNDEFINED: u8 = 0x06;
const MARKER_ECMA_ARRAY: u8 = 0x08;
const MARKER_OBJECT_END: u8 = 0x09;
const MARKER_STRICT_ARRAY: u8 = 0x0A;
const MARKER_LONG_STRING: u8 = 0x0C;

/// Encoding or decoding error.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Error {
    /// The input ended in the middle of a value.
    UnexpectedEof,
    /// A type marker this decoder does not implement.
    UnknownMarker(u8),
    /// A string that is not valid UTF-8.
    InvalidUtf8,
    /// Nesting deeper than [`Limits::max_depth`].
    TooDeep,
    /// More values than [`Limits::max_nodes`] allows.
    TooLarge,
    /// A property name longer than the 16-bit length field allows.
    NameTooLong(usize),
    /// A string longer than the 32-bit length field allows.
    StringTooLong(usize),
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::UnexpectedEof => write!(f, "AMF0 value is truncated"),
            Error::UnknownMarker(m) => write!(f, "unsupported AMF0 type marker 0x{m:02x}"),
            Error::InvalidUtf8 => write!(f, "AMF0 string is not valid UTF-8"),
            Error::TooDeep => write!(f, "AMF0 value is nested too deeply"),
            Error::TooLarge => write!(f, "AMF0 value has too many elements"),
            Error::NameTooLong(n) => write!(f, "AMF0 property name of {n} bytes is too long"),
            Error::StringTooLong(n) => write!(f, "AMF0 string of {n} bytes is too long"),
        }
    }
}

impl std::error::Error for Error {}

/// Appends the encoding of `value` to `out`. On error nothing is appended.
pub fn write_value(value: &Value, out: &mut Vec<u8>) -> Result<(), Error> {
    let start = out.len();
    let result = write_value_inner(value, out);
    if result.is_err() {
        out.truncate(start);
    }
    result
}

fn write_value_inner(value: &Value, out: &mut Vec<u8>) -> Result<(), Error> {
    match value {
        Value::Number(n) => {
            out.push(MARKER_NUMBER);
            out.extend_from_slice(&n.to_be_bytes());
        }
        Value::Boolean(b) => {
            out.push(MARKER_BOOLEAN);
            out.push(u8::from(*b));
        }
        Value::String(s) => write_string_inner(s, out)?,
        Value::Object(props) => {
            out.push(MARKER_OBJECT);
            write_properties(props, out)?;
        }
        Value::Null => out.push(MARKER_NULL),
        Value::Undefined => out.push(MARKER_UNDEFINED),
        Value::EcmaArray(props) => {
            out.push(MARKER_ECMA_ARRAY);
            out.extend_from_slice(&count_u32(props.len())?.to_be_bytes());
            write_properties(props, out)?;
        }
        Value::StrictArray(items) => {
            out.push(MARKER_STRICT_ARRAY);
            out.extend_from_slice(&count_u32(items.len())?.to_be_bytes());
            for item in items {
                write_value_inner(item, out)?;
            }
        }
    }
    Ok(())
}

/// Appends a string value (short or long form depending on its length). On error
/// nothing is appended.
pub fn write_string(s: &str, out: &mut Vec<u8>) -> Result<(), Error> {
    let start = out.len();
    let result = write_string_inner(s, out);
    if result.is_err() {
        out.truncate(start);
    }
    result
}

fn write_string_inner(s: &str, out: &mut Vec<u8>) -> Result<(), Error> {
    if let Ok(len) = u16::try_from(s.len()) {
        out.push(MARKER_STRING);
        out.extend_from_slice(&len.to_be_bytes());
    } else {
        let len = u32::try_from(s.len()).map_err(|_| Error::StringTooLong(s.len()))?;
        out.push(MARKER_LONG_STRING);
        out.extend_from_slice(&len.to_be_bytes());
    }
    out.extend_from_slice(s.as_bytes());
    Ok(())
}

fn write_properties(props: &[(String, Value)], out: &mut Vec<u8>) -> Result<(), Error> {
    for (name, value) in props {
        let len = u16::try_from(name.len()).map_err(|_| Error::NameTooLong(name.len()))?;
        out.extend_from_slice(&len.to_be_bytes());
        out.extend_from_slice(name.as_bytes());
        write_value_inner(value, out)?;
    }
    out.extend_from_slice(&[0, 0, MARKER_OBJECT_END]);
    Ok(())
}

fn count_u32(len: usize) -> Result<u32, Error> {
    u32::try_from(len).map_err(|_| Error::TooLarge)
}

/// Decodes one value from the start of `input` with the default [`Limits`], returning it
/// with the number of bytes consumed.
pub fn read_value(input: &[u8]) -> Result<(Value, usize), Error> {
    read_value_with(input, Limits::default())
}

/// Decodes one value from the start of `input` with explicit limits.
pub fn read_value_with(input: &[u8], limits: Limits) -> Result<(Value, usize), Error> {
    let mut budget = Budget {
        depth: 0,
        nodes_left: limits.max_nodes,
        max_depth: limits.max_depth,
    };
    read_node(input, &mut budget)
}

struct Budget {
    depth: usize,
    nodes_left: usize,
    max_depth: usize,
}

impl Budget {
    fn node(&mut self) -> Result<(), Error> {
        if self.nodes_left == 0 {
            return Err(Error::TooLarge);
        }
        self.nodes_left -= 1;
        Ok(())
    }

    fn enter(&mut self) -> Result<(), Error> {
        if self.depth >= self.max_depth {
            return Err(Error::TooDeep);
        }
        self.depth += 1;
        Ok(())
    }

    fn leave(&mut self) {
        self.depth -= 1;
    }
}

fn read_node(input: &[u8], budget: &mut Budget) -> Result<(Value, usize), Error> {
    budget.node()?;
    let marker = *input.first().ok_or(Error::UnexpectedEof)?;
    let rest = &input[1..];
    match marker {
        MARKER_NUMBER => {
            let bytes = take(rest, 8)?;
            let n = f64::from_be_bytes(bytes.try_into().expect("8 bytes"));
            Ok((Value::Number(n), 9))
        }
        MARKER_BOOLEAN => Ok((Value::Boolean(take(rest, 1)?[0] != 0), 2)),
        MARKER_STRING => {
            let (s, n) = read_short_string(rest)?;
            Ok((Value::String(s), 1 + n))
        }
        MARKER_LONG_STRING => {
            let len = u32::from_be_bytes(take(rest, 4)?.try_into().expect("4 bytes"));
            let len = usize::try_from(len).map_err(|_| Error::TooLarge)?;
            let bytes = take(&rest[4..], len)?;
            let s = std::str::from_utf8(bytes).map_err(|_| Error::InvalidUtf8)?;
            Ok((Value::String(s.to_owned()), 1 + 4 + len))
        }
        MARKER_OBJECT => {
            budget.enter()?;
            let (props, n) = read_properties(rest, budget)?;
            budget.leave();
            Ok((Value::Object(props), 1 + n))
        }
        MARKER_ECMA_ARRAY => {
            take(rest, 4)?;
            budget.enter()?;
            let (props, n) = read_properties(&rest[4..], budget)?;
            budget.leave();
            Ok((Value::EcmaArray(props), 1 + 4 + n))
        }
        MARKER_STRICT_ARRAY => {
            let count = u32::from_be_bytes(take(rest, 4)?.try_into().expect("4 bytes"));
            budget.enter()?;
            let mut pos = 4;
            // Never sized from `count`: the budget bounds the element count, the input
            // bounds the bytes.
            let mut items = Vec::new();
            for _ in 0..count {
                let (value, n) = read_node(&rest[pos..], budget)?;
                items.push(value);
                pos += n;
            }
            budget.leave();
            Ok((Value::StrictArray(items), 1 + pos))
        }
        MARKER_NULL => Ok((Value::Null, 1)),
        MARKER_UNDEFINED => Ok((Value::Undefined, 1)),
        other => Err(Error::UnknownMarker(other)),
    }
}

fn read_properties(
    input: &[u8],
    budget: &mut Budget,
) -> Result<(Vec<(String, Value)>, usize), Error> {
    let mut pos = 0;
    let mut props = Vec::new();
    loop {
        let head = take(&input[pos..], 3)?;
        if head == [0, 0, MARKER_OBJECT_END] {
            return Ok((props, pos + 3));
        }
        budget.node()?;
        let (name, n) = read_short_string(&input[pos..])?;
        pos += n;
        let (value, n) = read_node(&input[pos..], budget)?;
        pos += n;
        props.push((name, value));
    }
}

fn read_short_string(input: &[u8]) -> Result<(String, usize), Error> {
    let len = usize::from(u16::from_be_bytes(
        take(input, 2)?.try_into().expect("2 bytes"),
    ));
    let bytes = take(&input[2..], len)?;
    let s = std::str::from_utf8(bytes).map_err(|_| Error::InvalidUtf8)?;
    Ok((s.to_owned(), 2 + len))
}

fn take(input: &[u8], len: usize) -> Result<&[u8], Error> {
    input.get(..len).ok_or(Error::UnexpectedEof)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn props(items: &[(&str, Value)]) -> Vec<(String, Value)> {
        items
            .iter()
            .map(|(n, v)| ((*n).to_owned(), v.clone()))
            .collect()
    }

    fn encode(value: &Value) -> Vec<u8> {
        let mut bytes = Vec::new();
        write_value(value, &mut bytes).unwrap();
        bytes
    }

    #[test]
    fn round_trips_every_supported_type() {
        let value = Value::EcmaArray(props(&[
            ("duration", Value::Number(0.0)),
            ("stereo", Value::Boolean(true)),
            ("encoder", Value::String("prostor".into())),
            (
                "nested",
                Value::Object(props(&[("n", Value::Null), ("u", Value::Undefined)])),
            ),
            (
                "list",
                Value::StrictArray(vec![Value::Number(1.0), Value::String("x".into())]),
            ),
            ("long", Value::String("y".repeat(70_000))),
        ]));
        let bytes = encode(&value);
        let (decoded, consumed) = read_value(&bytes).unwrap();
        assert_eq!(consumed, bytes.len());
        assert_eq!(decoded, value);
    }

    #[test]
    fn encodes_like_the_spec_examples() {
        let mut bytes = Vec::new();
        write_value(&Value::Number(1.0), &mut bytes).unwrap();
        assert_eq!(bytes, [0x00, 0x3f, 0xf0, 0, 0, 0, 0, 0, 0]);

        bytes.clear();
        write_string("onMetaData", &mut bytes).unwrap();
        assert_eq!(&bytes[..3], &[0x02, 0x00, 0x0a]);
        assert_eq!(&bytes[3..], b"onMetaData");
    }

    #[test]
    fn short_and_long_strings_switch_at_65535_bytes() {
        let short = encode(&Value::String("s".repeat(65_535)));
        assert_eq!(short[0], MARKER_STRING);
        assert_eq!(&short[1..3], &[0xFF, 0xFF]);
        let long = encode(&Value::String("s".repeat(65_536)));
        assert_eq!(long[0], MARKER_LONG_STRING);
        assert_eq!(&long[1..5], &[0, 1, 0, 0]);
        for (bytes, header_len) in [(short, 3), (long, 5)] {
            let (value, n) = read_value(&bytes).unwrap();
            assert_eq!(n, bytes.len());
            assert_eq!(value.as_str().map(str::len), Some(bytes.len() - header_len));
        }
    }

    #[test]
    fn property_names_longer_than_65535_bytes_are_an_error_not_a_wrapped_length() {
        let value = Value::Object(props(&[(&"n".repeat(70_000), Value::Null)]));
        let mut bytes = vec![0xEE];
        assert_eq!(
            write_value(&value, &mut bytes),
            Err(Error::NameTooLong(70_000))
        );
        assert_eq!(bytes, [0xEE], "nothing appended on error");
    }

    #[test]
    fn reports_truncation_and_unknown_markers() {
        assert_eq!(read_value(&[]), Err(Error::UnexpectedEof));
        assert_eq!(read_value(&[0x00, 1, 2]), Err(Error::UnexpectedEof));
        assert_eq!(read_value(&[0x03, 0, 1, b'a']), Err(Error::UnexpectedEof));
        assert_eq!(
            read_value(&[0x07]),
            Err(Error::UnknownMarker(0x07)),
            "reference"
        );
        assert_eq!(read_value(&[0x0B]), Err(Error::UnknownMarker(0x0B)), "date");
    }

    #[test]
    fn every_prefix_of_an_ecma_array_is_truncated() {
        let bytes = encode(&Value::EcmaArray(props(&[
            ("width", Value::Number(1280.0)),
            ("encoder", Value::String("x".into())),
            (
                "inner",
                Value::Object(props(&[("k", Value::Boolean(true))])),
            ),
        ])));
        for cut in 0..bytes.len() {
            assert_eq!(
                read_value(&bytes[..cut]),
                Err(Error::UnexpectedEof),
                "cut at {cut}"
            );
        }
        assert_eq!(read_value(&bytes).unwrap().1, bytes.len());
    }

    #[test]
    fn rejects_invalid_utf8_in_strings_and_names() {
        assert_eq!(
            read_value(&[MARKER_STRING, 0, 2, 0xFF, 0xFE]),
            Err(Error::InvalidUtf8)
        );
        assert_eq!(
            read_value(&[MARKER_LONG_STRING, 0, 0, 0, 1, 0xC0]),
            Err(Error::InvalidUtf8)
        );
        assert_eq!(
            read_value(&[MARKER_OBJECT, 0, 1, 0xFF, MARKER_NULL, 0, 0, 9]),
            Err(Error::InvalidUtf8)
        );
    }

    #[test]
    fn deep_nesting_is_rejected_before_the_stack_runs_out() {
        // 10 000 nested strict arrays of one element each: 5 bytes per level.
        let mut bytes = Vec::new();
        for _ in 0..10_000 {
            bytes.extend_from_slice(&[MARKER_STRICT_ARRAY, 0, 0, 0, 1]);
        }
        bytes.push(MARKER_NULL);
        assert_eq!(read_value(&bytes), Err(Error::TooDeep));

        let mut objects = Vec::new();
        for _ in 0..10_000 {
            objects.extend_from_slice(&[MARKER_OBJECT, 0, 1, b'a']);
        }
        assert_eq!(read_value(&objects), Err(Error::TooDeep));

        // Exactly the limit is fine.
        let mut ok = Vec::new();
        for _ in 0..MAX_DEPTH {
            ok.extend_from_slice(&[MARKER_STRICT_ARRAY, 0, 0, 0, 1]);
        }
        ok.push(MARKER_NULL);
        assert!(read_value(&ok).is_ok());
    }

    #[test]
    fn huge_element_counts_do_not_allocate_or_run_away() {
        // Strict array claiming 2^32-1 elements, carrying three nulls.
        let bytes = [MARKER_STRICT_ARRAY, 0xFF, 0xFF, 0xFF, 0xFF, 5, 5, 5];
        assert_eq!(read_value(&bytes), Err(Error::UnexpectedEof));

        // Enough nulls to blow the node budget.
        let mut many = vec![MARKER_STRICT_ARRAY, 0xFF, 0xFF, 0xFF, 0xFF];
        many.extend(std::iter::repeat_n(MARKER_NULL, MAX_NODES + 10));
        assert_eq!(read_value(&many), Err(Error::TooLarge));

        // The same through an object with tiny properties.
        let mut object = vec![MARKER_OBJECT];
        for _ in 0..MAX_NODES {
            object.extend_from_slice(&[0, 0, MARKER_NULL]);
        }
        assert_eq!(read_value(&object), Err(Error::TooLarge));
    }

    #[test]
    fn custom_limits_apply() {
        let value = Value::StrictArray(vec![Value::Null; 10]);
        let bytes = encode(&value);
        let tight = Limits {
            max_depth: 1,
            max_nodes: 5,
        };
        assert_eq!(read_value_with(&bytes, tight), Err(Error::TooLarge));
        let flat = Limits {
            max_depth: 0,
            max_nodes: 100,
        };
        assert_eq!(read_value_with(&bytes, flat), Err(Error::TooDeep));
        assert_eq!(
            read_value_with(&[MARKER_NULL], flat).unwrap().0,
            Value::Null
        );
    }

    #[test]
    fn get_finds_properties_by_name() {
        let value = Value::Object(props(&[("width", Value::Number(1280.0))]));
        assert_eq!(value.get("width").and_then(Value::as_f64), Some(1280.0));
        assert_eq!(value.get("height"), None);
    }
}
