//! Strict JSON parser and CanonicalV1 bytes.
//!
//! Duplicate keys are rejected here. `serde_json::Value` last-key-wins is not
//! used as the parser.

use serde::Serialize;
use serde_json::{Map, Number, Value};

use crate::model::{InternalReason, ParseLimits, RtError, RtResult};

pub fn to_hex(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        out.push(HEX[(byte >> 4) as usize] as char);
        out.push(HEX[(byte & 0x0f) as usize] as char);
    }
    out
}

pub fn map_serde(err: serde_json::Error) -> RtError {
    let text = err.to_string();
    let reason = if text.contains("unknown_strategy") {
        InternalReason::UnknownStrategy
    } else if text.contains("safe_integer") {
        InternalReason::SafeInteger
    } else if text.contains("null_not_allowed") {
        InternalReason::NullNotAllowed
    } else if text.contains("wire_u64") {
        InternalReason::WireU64
    } else if text.contains("unknown field") {
        InternalReason::UnknownField
    } else if text.contains("missing field") {
        InternalReason::MissingField
    } else {
        InternalReason::InvalidJson
    };
    RtError::from_reason(reason)
}

pub fn parse_strict_json(bytes: &[u8], limits: &ParseLimits) -> RtResult<Value> {
    if bytes.len() > limits.max_bytes {
        return Err(RtError::from_reason(InternalReason::ByteLimit));
    }
    if bytes.starts_with(&[0xef, 0xbb, 0xbf]) {
        return Err(RtError::from_reason(InternalReason::InvalidUtf8));
    }
    let mut parser = Parser {
        input: bytes,
        index: 0,
        depth: 0,
        max_depth: limits.max_depth,
    };
    let value = parser.parse_value()?;
    parser.skip_ws();
    if parser.index != parser.input.len() {
        return Err(RtError::from_reason(InternalReason::TrailingData));
    }
    Ok(value)
}

pub fn canonical_bytes<T: Serialize>(value: &T) -> RtResult<Vec<u8>> {
    let value = serde_json::to_value(value).map_err(map_serde)?;
    let mut out = String::new();
    encode_value(&value, &mut out)?;
    Ok(out.into_bytes())
}

struct Parser<'a> {
    input: &'a [u8],
    index: usize,
    depth: u32,
    max_depth: u32,
}

impl<'a> Parser<'a> {
    fn parse_value(&mut self) -> RtResult<Value> {
        self.skip_ws();
        let byte = self
            .peek()
            .ok_or_else(|| RtError::from_reason(InternalReason::InvalidJson))?;
        match byte {
            b'{' => self.parse_object(),
            b'[' => self.parse_array(),
            b'"' => Ok(Value::String(self.parse_string()?)),
            b't' => self.literal(b"true", Value::Bool(true)),
            b'f' => self.literal(b"false", Value::Bool(false)),
            b'n' => self.literal(b"null", Value::Null),
            b'N' | b'I' => self.non_finite(),
            b'-' | b'0'..=b'9' => self.parse_number(),
            _ => Err(RtError::from_reason(InternalReason::InvalidJson)),
        }
    }

    fn parse_object(&mut self) -> RtResult<Value> {
        self.bump_depth()?;
        self.bump()
            .ok_or_else(|| RtError::from_reason(InternalReason::InvalidJson))?;
        let mut map = Map::new();
        self.skip_ws();
        if self.consume(b'}') {
            self.depth -= 1;
            return Ok(Value::Object(map));
        }
        loop {
            self.skip_ws();
            if self.peek() != Some(b'"') {
                return Err(RtError::from_reason(InternalReason::InvalidJson));
            }
            let key = self.parse_string()?;
            self.skip_ws();
            if !self.consume(b':') {
                return Err(RtError::from_reason(InternalReason::InvalidJson));
            }
            let value = self.parse_value()?;
            if map.contains_key(&key) {
                return Err(RtError::from_reason(InternalReason::DuplicateKey));
            }
            map.insert(key, value);
            self.skip_ws();
            if self.consume(b',') {
                continue;
            }
            if self.consume(b'}') {
                self.depth -= 1;
                return Ok(Value::Object(map));
            }
            return Err(RtError::from_reason(InternalReason::InvalidJson));
        }
    }

    fn parse_array(&mut self) -> RtResult<Value> {
        self.bump_depth()?;
        self.bump()
            .ok_or_else(|| RtError::from_reason(InternalReason::InvalidJson))?;
        let mut items = Vec::new();
        self.skip_ws();
        if self.consume(b']') {
            self.depth -= 1;
            return Ok(Value::Array(items));
        }
        loop {
            items.push(self.parse_value()?);
            self.skip_ws();
            if self.consume(b',') {
                continue;
            }
            if self.consume(b']') {
                self.depth -= 1;
                return Ok(Value::Array(items));
            }
            return Err(RtError::from_reason(InternalReason::InvalidJson));
        }
    }

    fn parse_string(&mut self) -> RtResult<String> {
        if !self.consume(b'"') {
            return Err(RtError::from_reason(InternalReason::InvalidJson));
        }
        let mut out = String::new();
        loop {
            let byte = self
                .bump()
                .ok_or_else(|| RtError::from_reason(InternalReason::InvalidJson))?;
            match byte {
                b'"' => return Ok(out),
                b'\\' => out.push(self.parse_escape()?),
                0x00..=0x1f => return Err(RtError::from_reason(InternalReason::InvalidJson)),
                0x20..=0x7f => out.push(byte as char),
                _ => {
                    self.index -= 1;
                    out.push(self.decode_utf8()?);
                }
            }
        }
    }

    fn parse_escape(&mut self) -> RtResult<char> {
        let byte = self
            .bump()
            .ok_or_else(|| RtError::from_reason(InternalReason::InvalidJson))?;
        match byte {
            b'"' => Ok('"'),
            b'\\' => Ok('\\'),
            b'/' => Ok('/'),
            b'b' => Ok('\u{0008}'),
            b'f' => Ok('\u{000c}'),
            b'n' => Ok('\n'),
            b'r' => Ok('\r'),
            b't' => Ok('\t'),
            b'u' => self.parse_unicode_escape(),
            _ => Err(RtError::from_reason(InternalReason::InvalidJson)),
        }
    }

    fn parse_unicode_escape(&mut self) -> RtResult<char> {
        let code = self.hex4()?;
        if (0xdc00..=0xdfff).contains(&code) {
            return Err(RtError::from_reason(InternalReason::LoneSurrogate));
        }
        if (0xd800..=0xdbff).contains(&code) {
            if self.bump() != Some(b'\\') || self.bump() != Some(b'u') {
                return Err(RtError::from_reason(InternalReason::LoneSurrogate));
            }
            let low = self.hex4()?;
            if !(0xdc00..=0xdfff).contains(&low) {
                return Err(RtError::from_reason(InternalReason::LoneSurrogate));
            }
            let scalar = 0x10000 + (((code - 0xd800) << 10) | (low - 0xdc00));
            return char::from_u32(scalar)
                .ok_or_else(|| RtError::from_reason(InternalReason::LoneSurrogate));
        }
        char::from_u32(code).ok_or_else(|| RtError::from_reason(InternalReason::InvalidJson))
    }

    fn parse_number(&mut self) -> RtResult<Value> {
        let start = self.index;
        let negative = self.consume(b'-');
        if negative && matches!(self.peek(), Some(b'I') | Some(b'N')) {
            return self.non_finite();
        }
        let first = self
            .bump()
            .ok_or_else(|| RtError::from_reason(InternalReason::InvalidNumber))?;
        if !first.is_ascii_digit() {
            return Err(RtError::from_reason(InternalReason::InvalidNumber));
        }
        if first == b'0' {
            if self.peek().is_some_and(|byte| byte.is_ascii_digit()) {
                return Err(RtError::from_reason(InternalReason::InvalidNumber));
            }
        } else {
            while self.peek().is_some_and(|byte| byte.is_ascii_digit()) {
                self.index += 1;
            }
        }
        if matches!(self.peek(), Some(b'.') | Some(b'e') | Some(b'E')) {
            return Err(RtError::from_reason(InternalReason::FloatRejected));
        }
        let text = std::str::from_utf8(&self.input[start..self.index])
            .map_err(|_| RtError::from_reason(InternalReason::InvalidUtf8))?;
        if text == "-0" {
            return Err(RtError::from_reason(InternalReason::NegativeZero));
        }
        if negative {
            let value = text
                .parse::<i64>()
                .map_err(|_| RtError::from_reason(InternalReason::InvalidNumber))?;
            Ok(Value::Number(Number::from(value)))
        } else {
            let value = text
                .parse::<u64>()
                .map_err(|_| RtError::from_reason(InternalReason::InvalidNumber))?;
            Ok(Value::Number(Number::from(value)))
        }
    }

    fn non_finite(&mut self) -> RtResult<Value> {
        if self.starts_with(b"NaN") {
            self.index += 3;
            return Err(RtError::from_reason(InternalReason::NonFiniteNumber));
        }
        if self.starts_with(b"Infinity") {
            self.index += 8;
            return Err(RtError::from_reason(InternalReason::NonFiniteNumber));
        }
        Err(RtError::from_reason(InternalReason::NonFiniteNumber))
    }

    fn literal(&mut self, text: &[u8], value: Value) -> RtResult<Value> {
        if self.starts_with(text) {
            self.index += text.len();
            Ok(value)
        } else {
            Err(RtError::from_reason(InternalReason::InvalidJson))
        }
    }

    fn bump_depth(&mut self) -> RtResult<()> {
        self.depth += 1;
        if self.depth > self.max_depth {
            Err(RtError::from_reason(InternalReason::MaxDepth))
        } else {
            Ok(())
        }
    }

    fn hex4(&mut self) -> RtResult<u32> {
        let mut value = 0u32;
        for _ in 0..4 {
            let byte = self
                .bump()
                .ok_or_else(|| RtError::from_reason(InternalReason::InvalidJson))?;
            let digit = match byte {
                b'0'..=b'9' => byte - b'0',
                b'a'..=b'f' => byte - b'a' + 10,
                b'A'..=b'F' => byte - b'A' + 10,
                _ => return Err(RtError::from_reason(InternalReason::InvalidJson)),
            };
            value = (value << 4) | u32::from(digit);
        }
        Ok(value)
    }

    fn decode_utf8(&mut self) -> RtResult<char> {
        let start = self.index;
        let first = self
            .bump()
            .ok_or_else(|| RtError::from_reason(InternalReason::InvalidUtf8))?;
        let width = match first {
            0xc2..=0xdf => 2,
            0xe0..=0xef => 3,
            0xf0..=0xf4 => 4,
            _ => return Err(RtError::from_reason(InternalReason::InvalidUtf8)),
        };
        let mut buf = [first, 0, 0, 0];
        for slot in buf.iter_mut().take(width).skip(1) {
            *slot = self
                .bump()
                .ok_or_else(|| RtError::from_reason(InternalReason::InvalidUtf8))?;
        }
        let text = std::str::from_utf8(&buf[..width]).map_err(|_| {
            self.index = start;
            RtError::from_reason(InternalReason::InvalidUtf8)
        })?;
        text.chars()
            .next()
            .ok_or_else(|| RtError::from_reason(InternalReason::InvalidUtf8))
    }

    fn skip_ws(&mut self) {
        while matches!(self.peek(), Some(b' ' | b'\n' | b'\r' | b'\t')) {
            self.index += 1;
        }
    }

    fn peek(&self) -> Option<u8> {
        self.input.get(self.index).copied()
    }

    fn bump(&mut self) -> Option<u8> {
        let byte = self.peek()?;
        self.index += 1;
        Some(byte)
    }

    fn consume(&mut self, expected: u8) -> bool {
        if self.peek() == Some(expected) {
            self.index += 1;
            true
        } else {
            false
        }
    }

    fn starts_with(&self, text: &[u8]) -> bool {
        self.input[self.index..].starts_with(text)
    }
}

fn encode_value(value: &Value, out: &mut String) -> RtResult<()> {
    match value {
        Value::Null => out.push_str("null"),
        Value::Bool(true) => out.push_str("true"),
        Value::Bool(false) => out.push_str("false"),
        Value::Number(number) => encode_number(number, out)?,
        Value::String(text) => push_string(out, text),
        Value::Array(items) => {
            out.push('[');
            for (index, item) in items.iter().enumerate() {
                if index > 0 {
                    out.push(',');
                }
                encode_value(item, out)?;
            }
            out.push(']');
        }
        Value::Object(map) => {
            let mut keys: Vec<&String> = map.keys().collect();
            keys.sort();
            out.push('{');
            for (index, key) in keys.iter().enumerate() {
                if index > 0 {
                    out.push(',');
                }
                push_string(out, key);
                out.push(':');
                encode_value(&map[*key], out)?;
            }
            out.push('}');
        }
    }
    Ok(())
}

fn encode_number(number: &Number, out: &mut String) -> RtResult<()> {
    if let Some(value) = number.as_u64() {
        out.push_str(&value.to_string());
        Ok(())
    } else if let Some(value) = number.as_i64() {
        out.push_str(&value.to_string());
        Ok(())
    } else {
        Err(RtError::from_reason(InternalReason::FloatRejected))
    }
}

fn push_string(out: &mut String, text: &str) {
    out.push('"');
    for ch in text.chars() {
        match ch {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            c if (c as u32) <= 0x1f => {
                let code = u32::from(c);
                out.push_str(&format!("\\u{code:04x}"));
            }
            c => out.push(c),
        }
    }
    out.push('"');
}
