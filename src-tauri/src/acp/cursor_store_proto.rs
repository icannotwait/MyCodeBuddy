use serde_json::Value;

const MAX_MESSAGE_DEPTH: usize = 24;
const MAX_VALUE_DEPTH: usize = 32;
const MAX_VISITED_ROWS: usize = 100_000;
const MAX_VISITED_BYTES: usize = 128 * 1024 * 1024;
const MAX_VISITED_FIELDS: usize = 50_000;
const MAX_CANDIDATES: usize = 256;

#[derive(Debug, Clone, PartialEq)]
pub(super) struct ProtobufMcpToolCall {
    pub tool_name: String,
    pub args: Value,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum ProtobufScanError {
    MalformedAnchoredRecord,
    WorkLimitExceeded,
}

#[derive(Debug, Clone, Copy)]
enum WireValue<'a> {
    Varint(u64),
    Fixed64(u64),
    Bytes(&'a [u8]),
    Fixed32,
}

#[derive(Debug, Clone, Copy)]
struct Field<'a> {
    number: u32,
    value: WireValue<'a>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DecodeError {
    Malformed,
    MalformedAnchoredRecord,
    WorkLimitExceeded,
}

#[derive(Default)]
pub(super) struct ProtobufScanBudget {
    visited_rows: usize,
    visited_bytes: usize,
    visited_fields: usize,
    candidates: usize,
}

impl ProtobufScanBudget {
    pub(super) fn record_store_row(&mut self) -> Result<(), ProtobufScanError> {
        self.record_row()
            .map_err(|_| ProtobufScanError::WorkLimitExceeded)
    }

    pub(super) fn record_store_bytes(&mut self, len: usize) -> Result<(), ProtobufScanError> {
        self.record_bytes(len)
            .map_err(|_| ProtobufScanError::WorkLimitExceeded)
    }

    pub(super) fn record_json_candidate(&mut self) -> Result<(), ProtobufScanError> {
        self.record_candidate()
            .map_err(|_| ProtobufScanError::WorkLimitExceeded)
    }

    fn record_row(&mut self) -> Result<(), DecodeError> {
        self.visited_rows = self
            .visited_rows
            .checked_add(1)
            .ok_or(DecodeError::WorkLimitExceeded)?;
        if self.visited_rows > MAX_VISITED_ROWS {
            return Err(DecodeError::WorkLimitExceeded);
        }
        Ok(())
    }

    fn record_bytes(&mut self, len: usize) -> Result<(), DecodeError> {
        self.visited_bytes = self
            .visited_bytes
            .checked_add(len)
            .ok_or(DecodeError::WorkLimitExceeded)?;
        if self.visited_bytes > MAX_VISITED_BYTES {
            return Err(DecodeError::WorkLimitExceeded);
        }
        Ok(())
    }

    fn record_field(&mut self) -> Result<(), DecodeError> {
        self.visited_fields = self
            .visited_fields
            .checked_add(1)
            .ok_or(DecodeError::WorkLimitExceeded)?;
        if self.visited_fields > MAX_VISITED_FIELDS {
            return Err(DecodeError::WorkLimitExceeded);
        }
        Ok(())
    }

    fn record_candidate(&mut self) -> Result<(), DecodeError> {
        self.candidates = self
            .candidates
            .checked_add(1)
            .ok_or(DecodeError::WorkLimitExceeded)?;
        if self.candidates > MAX_CANDIDATES {
            return Err(DecodeError::WorkLimitExceeded);
        }
        Ok(())
    }
}

/// Finds complete `agent.v1.McpArgs` messages nested in a Cursor blob.
///
/// Length-delimited protobuf fields can be strings, bytes, or child messages,
/// so descent is restricted to branches containing the exact requested tool
/// call id. A malformed branch is ignored; exhausting the work budget is
/// surfaced so callers can fail closed instead of returning a partial scan.
pub(super) fn find_mcp_tool_calls(
    data: &[u8],
    tool_call_id: &str,
    budget: &mut ProtobufScanBudget,
) -> Result<Vec<ProtobufMcpToolCall>, ProtobufScanError> {
    if tool_call_id.is_empty()
        || !contains_subslice(data, tool_call_id.as_bytes(), budget)
            .map_err(|_| ProtobufScanError::WorkLimitExceeded)?
    {
        return Ok(Vec::new());
    }

    let mut found = Vec::new();
    match scan_message(data, tool_call_id.as_bytes(), 0, budget, &mut found) {
        Ok(()) | Err(DecodeError::Malformed) => Ok(found),
        Err(DecodeError::MalformedAnchoredRecord) => {
            Err(ProtobufScanError::MalformedAnchoredRecord)
        }
        Err(DecodeError::WorkLimitExceeded) => Err(ProtobufScanError::WorkLimitExceeded),
    }
}

fn scan_message(
    data: &[u8],
    tool_call_id: &[u8],
    depth: usize,
    budget: &mut ProtobufScanBudget,
    found: &mut Vec<ProtobufMcpToolCall>,
) -> Result<(), DecodeError> {
    if depth > MAX_MESSAGE_DEPTH {
        return Err(DecodeError::WorkLimitExceeded);
    }

    let fields = parse_fields(data, budget)?;
    if has_exact_bytes_field(&fields, 3, tool_call_id) {
        let candidate =
            decode_mcp_args(&fields, tool_call_id, budget).map_err(|err| match err {
                DecodeError::Malformed => DecodeError::MalformedAnchoredRecord,
                other => other,
            })?;
        budget.record_candidate()?;
        found.push(candidate);
        return Ok(());
    }

    for field in fields {
        let WireValue::Bytes(child) = field.value else {
            continue;
        };
        if contains_subslice(child, tool_call_id, budget)? {
            match scan_message(child, tool_call_id, depth + 1, budget, found) {
                Ok(()) | Err(DecodeError::Malformed) => {}
                Err(err) => return Err(err),
            }
        }
    }
    Ok(())
}

fn decode_mcp_args(
    fields: &[Field<'_>],
    tool_call_id: &[u8],
    budget: &mut ProtobufScanBudget,
) -> Result<ProtobufMcpToolCall, DecodeError> {
    let mut id = None;
    let mut tool_name = None;
    let mut args = serde_json::Map::new();

    for field in fields {
        match field.number {
            2 => {
                let WireValue::Bytes(entry) = field.value else {
                    return Err(DecodeError::Malformed);
                };
                let (key, value) = decode_map_entry(entry, 0, budget)?;
                if args.insert(key, value).is_some() {
                    return Err(DecodeError::Malformed);
                }
            }
            3 => {
                let WireValue::Bytes(value) = field.value else {
                    return Err(DecodeError::Malformed);
                };
                if id.replace(value).is_some() {
                    return Err(DecodeError::Malformed);
                }
            }
            5 => {
                let WireValue::Bytes(value) = field.value else {
                    return Err(DecodeError::Malformed);
                };
                let value = std::str::from_utf8(value).map_err(|_| DecodeError::Malformed)?;
                if value.is_empty() || tool_name.replace(value).is_some() {
                    return Err(DecodeError::Malformed);
                }
            }
            _ => {}
        }
    }

    if id != Some(tool_call_id) {
        return Err(DecodeError::Malformed);
    }
    Ok(ProtobufMcpToolCall {
        tool_name: tool_name.ok_or(DecodeError::Malformed)?.to_owned(),
        args: Value::Object(args),
    })
}

fn decode_map_entry(
    data: &[u8],
    depth: usize,
    budget: &mut ProtobufScanBudget,
) -> Result<(String, Value), DecodeError> {
    let fields = parse_fields(data, budget)?;
    let mut key = None;
    let mut value = None;
    for field in fields {
        match field.number {
            1 => {
                let WireValue::Bytes(bytes) = field.value else {
                    return Err(DecodeError::Malformed);
                };
                let decoded = std::str::from_utf8(bytes).map_err(|_| DecodeError::Malformed)?;
                if key.replace(decoded.to_owned()).is_some() {
                    return Err(DecodeError::Malformed);
                }
            }
            2 => {
                let WireValue::Bytes(bytes) = field.value else {
                    return Err(DecodeError::Malformed);
                };
                let decoded = decode_proto_value(bytes, depth, budget)?;
                if value.replace(decoded).is_some() {
                    return Err(DecodeError::Malformed);
                }
            }
            _ => {}
        }
    }
    Ok((
        key.ok_or(DecodeError::Malformed)?,
        value.ok_or(DecodeError::Malformed)?,
    ))
}

fn decode_proto_value(
    data: &[u8],
    depth: usize,
    budget: &mut ProtobufScanBudget,
) -> Result<Value, DecodeError> {
    if depth > MAX_VALUE_DEPTH {
        return Err(DecodeError::WorkLimitExceeded);
    }
    let fields = parse_fields(data, budget)?;
    let mut decoded = None;

    for field in fields {
        let next = match field.number {
            1 => {
                let WireValue::Varint(value) = field.value else {
                    return Err(DecodeError::Malformed);
                };
                if value != 0 {
                    return Err(DecodeError::Malformed);
                }
                Some(Value::Null)
            }
            2 => {
                let WireValue::Fixed64(bits) = field.value else {
                    return Err(DecodeError::Malformed);
                };
                let number = json_number_from_f64(f64::from_bits(bits))?;
                Some(Value::Number(number))
            }
            3 => {
                let WireValue::Bytes(bytes) = field.value else {
                    return Err(DecodeError::Malformed);
                };
                Some(Value::String(
                    std::str::from_utf8(bytes)
                        .map_err(|_| DecodeError::Malformed)?
                        .to_owned(),
                ))
            }
            4 => {
                let WireValue::Varint(value) = field.value else {
                    return Err(DecodeError::Malformed);
                };
                match value {
                    0 => Some(Value::Bool(false)),
                    1 => Some(Value::Bool(true)),
                    _ => return Err(DecodeError::Malformed),
                }
            }
            5 => {
                let WireValue::Bytes(bytes) = field.value else {
                    return Err(DecodeError::Malformed);
                };
                Some(decode_proto_struct(bytes, depth + 1, budget)?)
            }
            6 => {
                let WireValue::Bytes(bytes) = field.value else {
                    return Err(DecodeError::Malformed);
                };
                Some(decode_proto_list(bytes, depth + 1, budget)?)
            }
            _ => None,
        };

        if let Some(next) = next {
            if decoded.replace(next).is_some() {
                return Err(DecodeError::Malformed);
            }
        }
    }
    decoded.ok_or(DecodeError::Malformed)
}

/// JavaScript JSON persistence writes integer-valued doubles such as `1.0`
/// as `1`. Canonicalize exactly representable integers the same way so an
/// otherwise identical JSON/protobuf pair does not look conflicting.
fn json_number_from_f64(value: f64) -> Result<serde_json::Number, DecodeError> {
    if !value.is_finite() {
        return Err(DecodeError::Malformed);
    }
    if value == 0.0 {
        return Ok(serde_json::Number::from(0));
    }
    if value.fract() == 0.0 {
        if value > 0.0 && value < u64::MAX as f64 {
            let integer = value as u64;
            if integer as f64 == value {
                return Ok(serde_json::Number::from(integer));
            }
        } else if value >= i64::MIN as f64 && value < 0.0 {
            let integer = value as i64;
            if integer as f64 == value {
                return Ok(serde_json::Number::from(integer));
            }
        }
    }
    serde_json::Number::from_f64(value).ok_or(DecodeError::Malformed)
}

fn decode_proto_struct(
    data: &[u8],
    depth: usize,
    budget: &mut ProtobufScanBudget,
) -> Result<Value, DecodeError> {
    let fields = parse_fields(data, budget)?;
    let mut object = serde_json::Map::new();
    for field in fields {
        if field.number != 1 {
            continue;
        }
        let WireValue::Bytes(entry) = field.value else {
            return Err(DecodeError::Malformed);
        };
        let (key, value) = decode_map_entry(entry, depth, budget)?;
        if object.insert(key, value).is_some() {
            return Err(DecodeError::Malformed);
        }
    }
    Ok(Value::Object(object))
}

fn decode_proto_list(
    data: &[u8],
    depth: usize,
    budget: &mut ProtobufScanBudget,
) -> Result<Value, DecodeError> {
    let fields = parse_fields(data, budget)?;
    let mut list = Vec::new();
    for field in fields {
        if field.number != 1 {
            continue;
        }
        let WireValue::Bytes(item) = field.value else {
            return Err(DecodeError::Malformed);
        };
        list.push(decode_proto_value(item, depth, budget)?);
    }
    Ok(Value::Array(list))
}

fn parse_fields<'a>(
    data: &'a [u8],
    budget: &mut ProtobufScanBudget,
) -> Result<Vec<Field<'a>>, DecodeError> {
    budget.record_bytes(data.len())?;
    let mut fields = Vec::new();
    let mut position = 0;
    while position < data.len() {
        budget.record_field()?;
        let key = read_varint(data, &mut position)?;
        let number = u32::try_from(key >> 3).map_err(|_| DecodeError::Malformed)?;
        if number == 0 || number > 0x1fff_ffff {
            return Err(DecodeError::Malformed);
        }
        let value = match key & 0x07 {
            0 => WireValue::Varint(read_varint(data, &mut position)?),
            1 => {
                let bytes = take(data, &mut position, 8)?;
                WireValue::Fixed64(u64::from_le_bytes(
                    bytes.try_into().map_err(|_| DecodeError::Malformed)?,
                ))
            }
            2 => {
                let len = usize::try_from(read_varint(data, &mut position)?)
                    .map_err(|_| DecodeError::Malformed)?;
                WireValue::Bytes(take(data, &mut position, len)?)
            }
            5 => {
                take(data, &mut position, 4)?;
                WireValue::Fixed32
            }
            _ => return Err(DecodeError::Malformed),
        };
        fields.push(Field { number, value });
    }
    Ok(fields)
}

fn read_varint(data: &[u8], position: &mut usize) -> Result<u64, DecodeError> {
    let mut value = 0_u64;
    for shift in (0..=63).step_by(7) {
        let byte = *data.get(*position).ok_or(DecodeError::Malformed)?;
        *position += 1;
        if shift == 63 && byte > 1 {
            return Err(DecodeError::Malformed);
        }
        value |= u64::from(byte & 0x7f) << shift;
        if byte & 0x80 == 0 {
            return Ok(value);
        }
    }
    Err(DecodeError::Malformed)
}

fn take<'a>(data: &'a [u8], position: &mut usize, len: usize) -> Result<&'a [u8], DecodeError> {
    let end = position.checked_add(len).ok_or(DecodeError::Malformed)?;
    let bytes = data.get(*position..end).ok_or(DecodeError::Malformed)?;
    *position = end;
    Ok(bytes)
}

fn has_exact_bytes_field(fields: &[Field<'_>], number: u32, expected: &[u8]) -> bool {
    fields.iter().any(|field| {
        field.number == number
            && matches!(field.value, WireValue::Bytes(value) if value == expected)
    })
}

fn contains_subslice(
    haystack: &[u8],
    needle: &[u8],
    budget: &mut ProtobufScanBudget,
) -> Result<bool, DecodeError> {
    budget.record_bytes(haystack.len())?;
    Ok(!needle.is_empty()
        && haystack
            .windows(needle.len())
            .any(|window| window == needle))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lookup_budget_is_shared_across_rows_and_work_dimensions() {
        let mut rows = ProtobufScanBudget {
            visited_rows: MAX_VISITED_ROWS - 1,
            ..ProtobufScanBudget::default()
        };
        assert!(rows.record_store_row().is_ok());
        assert_eq!(
            rows.record_store_row(),
            Err(ProtobufScanError::WorkLimitExceeded)
        );

        let mut bytes = ProtobufScanBudget {
            visited_bytes: MAX_VISITED_BYTES,
            ..ProtobufScanBudget::default()
        };
        assert_eq!(
            find_mcp_tool_calls(b"x", "target", &mut bytes),
            Err(ProtobufScanError::WorkLimitExceeded)
        );

        let mut fields = ProtobufScanBudget {
            visited_fields: MAX_VISITED_FIELDS,
            ..ProtobufScanBudget::default()
        };
        assert!(matches!(
            parse_fields(&[0x08, 0x00], &mut fields),
            Err(DecodeError::WorkLimitExceeded)
        ));

        let mut candidates = ProtobufScanBudget {
            candidates: MAX_CANDIDATES,
            ..ProtobufScanBudget::default()
        };
        assert_eq!(
            candidates.record_json_candidate(),
            Err(ProtobufScanError::WorkLimitExceeded)
        );
    }
}
