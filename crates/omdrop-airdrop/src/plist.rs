//! Binary property list (`bplist00`) reader and writer for AirDrop messages.

use std::collections::BTreeSet;

const HEADER: &[u8; 8] = b"bplist00";
const TRAILER_SIZE: usize = 32;
const MAX_OBJECTS: u64 = 1_000_000;
const MAX_DEPTH: usize = 64;

#[derive(Debug, Clone, PartialEq)]
pub enum Value {
    Null,
    Bool(bool),
    Integer(i64),
    Real(f64),
    /// Seconds since 2001-01-01 00:00:00 UTC.
    Date(f64),
    Data(Vec<u8>),
    String(String),
    Array(Vec<Value>),
    Dictionary(Vec<(String, Value)>),
    Uid(u64),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlistError(pub &'static str);

impl std::fmt::Display for PlistError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.0)
    }
}

impl std::error::Error for PlistError {}

/// Parse a binary property list. The parser bounds object count, recursion depth, and every table
/// access before reading bytes so malformed network payloads fail as values, not panics.
pub fn read(data: &[u8]) -> Result<Value, PlistError> {
    if data.len() < HEADER.len() + TRAILER_SIZE || data.get(..8) != Some(HEADER) {
        return Err(PlistError("not a binary plist"));
    }
    let trailer_at = data.len() - TRAILER_SIZE;
    let offset_size = data[trailer_at + 6] as usize;
    let reference_size = data[trailer_at + 7] as usize;
    let count = read_uint(data, trailer_at + 8, 8)?;
    let top = read_uint(data, trailer_at + 16, 8)?;
    let table_at = read_uint(data, trailer_at + 24, 8)?;

    if offset_size == 0
        || offset_size > 8
        || reference_size == 0
        || reference_size > 8
        || count == 0
        || count > MAX_OBJECTS
        || top >= count
        || table_at > trailer_at as u64
    {
        return Err(PlistError("bad plist trailer"));
    }
    let table_at = usize::try_from(table_at).map_err(|_| PlistError("bad plist offset table"))?;
    let table_bytes = usize::try_from(count)
        .ok()
        .and_then(|count| count.checked_mul(offset_size))
        .ok_or(PlistError("bad plist offset table"))?;
    if table_at
        .checked_add(table_bytes)
        .is_none_or(|end| end > trailer_at)
    {
        return Err(PlistError("bad plist offset table"));
    }

    let mut offsets = Vec::with_capacity(count as usize);
    for index in 0..count as usize {
        let offset = read_uint(data, table_at + index * offset_size, offset_size)?;
        if offset < HEADER.len() as u64 || offset >= table_at as u64 {
            return Err(PlistError("bad plist object offset"));
        }
        offsets.push(offset as usize);
    }
    let mut visiting = BTreeSet::new();
    Reader {
        data,
        offsets: &offsets,
        reference_size,
        table_at,
        visiting: &mut visiting,
    }
    .object(top, 0)
}

struct Reader<'a> {
    data: &'a [u8],
    offsets: &'a [usize],
    reference_size: usize,
    table_at: usize,
    visiting: &'a mut BTreeSet<u64>,
}

impl Reader<'_> {
    fn object(&mut self, index: u64, depth: usize) -> Result<Value, PlistError> {
        if depth > MAX_DEPTH || index as usize >= self.offsets.len() {
            return Err(PlistError("bad plist reference"));
        }
        if !self.visiting.insert(index) {
            return Err(PlistError("cyclic plist reference"));
        }
        let result = self.object_at(self.offsets[index as usize], depth);
        self.visiting.remove(&index);
        result
    }

    fn object_at(&mut self, at: usize, depth: usize) -> Result<Value, PlistError> {
        if at >= self.table_at {
            return Err(PlistError("bad plist object offset"));
        }
        let marker = self.data[at];
        let kind = marker >> 4;
        let info = marker & 0x0f;
        match kind {
            0x0 => match info {
                0x0 => Ok(Value::Null),
                0x8 => Ok(Value::Bool(false)),
                0x9 => Ok(Value::Bool(true)),
                _ => Err(PlistError("unsupported plist simple value")),
            },
            0x1 => {
                let size = integer_size(info)?;
                let raw = self.value_uint(at + 1, size)?;
                let value = if size == 8 {
                    raw as i64
                } else {
                    i64::try_from(raw).map_err(|_| PlistError("plist integer overflow"))?
                };
                Ok(Value::Integer(value))
            }
            0x2 => match info {
                0x2 => {
                    let bytes = self.value_bytes(at + 1, 4)?;
                    Ok(Value::Real(
                        f32::from_be_bytes(bytes.try_into().unwrap()) as f64
                    ))
                }
                0x3 => {
                    let bytes = self.value_bytes(at + 1, 8)?;
                    Ok(Value::Real(f64::from_be_bytes(bytes.try_into().unwrap())))
                }
                _ => Err(PlistError("unsupported plist real")),
            },
            0x3 if info == 0x3 => {
                let bytes = self.value_bytes(at + 1, 8)?;
                Ok(Value::Date(f64::from_be_bytes(bytes.try_into().unwrap())))
            }
            0x4 => {
                let (length, start) = self.length(at, info)?;
                Ok(Value::Data(self.value_bytes(start, length)?.to_vec()))
            }
            0x5 => {
                let (length, start) = self.length(at, info)?;
                let bytes = self.value_bytes(start, length)?;
                if !bytes.is_ascii() {
                    return Err(PlistError("invalid ASCII plist string"));
                }
                Ok(Value::String(
                    String::from_utf8(bytes.to_vec())
                        .map_err(|_| PlistError("invalid plist string"))?,
                ))
            }
            0x6 => {
                let (units, start) = self.length(at, info)?;
                let byte_len = units
                    .checked_mul(2)
                    .ok_or(PlistError("plist string too large"))?;
                let bytes = self.value_bytes(start, byte_len)?;
                let (pairs, _) = bytes.as_chunks::<2>();
                let words = pairs
                    .iter()
                    .map(|[high, low]| u16::from_be_bytes([*high, *low]));
                let string: Result<String, _> = char::decode_utf16(words).collect();
                string
                    .map(Value::String)
                    .map_err(|_| PlistError("invalid UTF-16 plist string"))
            }
            0x8 => {
                let size = info as usize + 1;
                if size > 8 {
                    return Err(PlistError("unsupported plist UID size"));
                }
                Ok(Value::Uid(self.value_uint(at + 1, size)?))
            }
            0xa => {
                let (count, start) = self.length(at, info)?;
                let references = self.references(start, count as u64)?;
                let mut values = Vec::with_capacity(references.len());
                for reference in references {
                    values.push(self.object(reference, depth + 1)?);
                }
                Ok(Value::Array(values))
            }
            0xd => {
                let (count, start) = self.length(at, info)?;
                let all_refs = count
                    .checked_mul(2)
                    .ok_or(PlistError("plist dictionary too large"))?;
                let references = self.references(start, all_refs as u64)?;
                let mut entries = Vec::with_capacity(count);
                for i in 0..count {
                    let key = self.object(references[i], depth + 1)?;
                    let Value::String(key) = key else {
                        return Err(PlistError("plist dictionary key is not a string"));
                    };
                    let value = self.object(references[count + i], depth + 1)?;
                    entries.push((key, value));
                }
                Ok(Value::Dictionary(entries))
            }
            _ => Err(PlistError("unsupported plist marker")),
        }
    }

    fn length(&self, at: usize, info: u8) -> Result<(usize, usize), PlistError> {
        if info != 0x0f {
            return Ok((info as usize, at + 1));
        }
        let marker = *self
            .data
            .get(at + 1)
            .ok_or(PlistError("truncated plist length"))?;
        if marker >> 4 != 0x1 {
            return Err(PlistError("invalid plist length"));
        }
        let size = integer_size(marker & 0x0f)?;
        let length = usize::try_from(self.value_uint(at + 2, size)?)
            .map_err(|_| PlistError("plist object too large"))?;
        Ok((length, at + 2 + size))
    }

    fn references(&self, start: usize, count: u64) -> Result<Vec<u64>, PlistError> {
        let count = usize::try_from(count).map_err(|_| PlistError("plist collection too large"))?;
        let bytes = count
            .checked_mul(self.reference_size)
            .ok_or(PlistError("plist collection too large"))?;
        self.value_bytes(start, bytes)?;
        let mut refs = Vec::with_capacity(count);
        for index in 0..count {
            refs.push(read_uint(
                self.data,
                start + index * self.reference_size,
                self.reference_size,
            )?);
        }
        Ok(refs)
    }

    fn value_bytes(&self, start: usize, length: usize) -> Result<&[u8], PlistError> {
        let end = start
            .checked_add(length)
            .ok_or(PlistError("plist object too large"))?;
        if end > self.table_at {
            return Err(PlistError("truncated plist object"));
        }
        Ok(&self.data[start..end])
    }

    fn value_uint(&self, start: usize, size: usize) -> Result<u64, PlistError> {
        read_uint(self.data, start, size)
    }
}

/// Serialize a plist value to the binary `bplist00` representation.
pub fn write(root: &Value) -> Result<Vec<u8>, PlistError> {
    let mut nodes = Vec::new();
    flatten(root, &mut nodes, 0)?;
    let reference_size = integer_width(nodes.len().saturating_sub(1) as u64);
    let mut output = HEADER.to_vec();
    let mut offsets = Vec::with_capacity(nodes.len());

    for node in &nodes {
        offsets.push(output.len() as u64);
        write_object(&mut output, node, reference_size)?;
    }
    let table_at = output.len() as u64;
    let offset_size = integer_width(table_at);
    for offset in offsets {
        write_uint(&mut output, offset, offset_size);
    }
    output.resize(output.len() + 6, 0);
    output.push(offset_size as u8);
    output.push(reference_size as u8);
    output.extend_from_slice(&(nodes.len() as u64).to_be_bytes());
    output.extend_from_slice(&0u64.to_be_bytes());
    output.extend_from_slice(&table_at.to_be_bytes());
    Ok(output)
}

struct Node {
    value: Value,
    children: Vec<usize>,
}

fn flatten(value: &Value, nodes: &mut Vec<Node>, depth: usize) -> Result<usize, PlistError> {
    if depth > MAX_DEPTH || nodes.len() as u64 >= MAX_OBJECTS {
        return Err(PlistError("plist is too large or deeply nested"));
    }
    let index = nodes.len();
    nodes.push(Node {
        value: value.clone(),
        children: Vec::new(),
    });
    let children = match value {
        Value::Array(values) => values
            .iter()
            .map(|child| flatten(child, nodes, depth + 1))
            .collect::<Result<Vec<_>, _>>()?,
        Value::Dictionary(entries) => {
            let mut children = Vec::with_capacity(entries.len() * 2);
            for (key, _) in entries {
                children.push(flatten(&Value::String(key.clone()), nodes, depth + 1)?);
            }
            for (_, child) in entries {
                children.push(flatten(child, nodes, depth + 1)?);
            }
            children
        }
        _ => Vec::new(),
    };
    nodes[index].children = children;
    Ok(index)
}

fn write_object(
    output: &mut Vec<u8>,
    node: &Node,
    reference_size: usize,
) -> Result<(), PlistError> {
    match &node.value {
        Value::Null => output.push(0x00),
        Value::Bool(false) => output.push(0x08),
        Value::Bool(true) => output.push(0x09),
        Value::Integer(value) if *value < 0 => {
            output.push(0x13);
            output.extend_from_slice(&value.to_be_bytes());
        }
        Value::Integer(value) => {
            let width = integer_width(*value as u64);
            output.push(0x10 | width.trailing_zeros() as u8);
            write_uint(output, *value as u64, width);
        }
        Value::Real(value) => {
            output.push(0x23);
            output.extend_from_slice(&value.to_be_bytes());
        }
        Value::Date(value) => {
            output.push(0x33);
            output.extend_from_slice(&value.to_be_bytes());
        }
        Value::Data(bytes) => {
            write_header(output, 0x4, bytes.len() as u64);
            output.extend_from_slice(bytes);
        }
        Value::String(text) if text.is_ascii() => {
            write_header(output, 0x5, text.len() as u64);
            output.extend_from_slice(text.as_bytes());
        }
        Value::String(text) => {
            let units: Vec<u16> = text.encode_utf16().collect();
            write_header(output, 0x6, units.len() as u64);
            for unit in units {
                output.extend_from_slice(&unit.to_be_bytes());
            }
        }
        Value::Array(_) => {
            write_header(output, 0xa, node.children.len() as u64);
            for &child in &node.children {
                write_uint(output, child as u64, reference_size);
            }
        }
        Value::Dictionary(entries) => {
            write_header(output, 0xd, entries.len() as u64);
            for &child in &node.children {
                write_uint(output, child as u64, reference_size);
            }
        }
        Value::Uid(value) => {
            let width = integer_width(*value);
            output.push(0x80 | (width as u8 - 1));
            write_uint(output, *value, width);
        }
    }
    Ok(())
}

fn write_header(output: &mut Vec<u8>, kind: u8, count: u64) {
    if count < 15 {
        output.push(kind << 4 | count as u8);
        return;
    }
    output.push(kind << 4 | 0x0f);
    let width = integer_width(count);
    output.push(0x10 | width.trailing_zeros() as u8);
    write_uint(output, count, width);
}

fn integer_size(info: u8) -> Result<usize, PlistError> {
    if info > 3 {
        return Err(PlistError("unsupported plist integer width"));
    }
    Ok(1usize << info)
}

fn integer_width(value: u64) -> usize {
    if value <= u8::MAX as u64 {
        1
    } else if value <= u16::MAX as u64 {
        2
    } else if value <= u32::MAX as u64 {
        4
    } else {
        8
    }
}

fn read_uint(data: &[u8], start: usize, size: usize) -> Result<u64, PlistError> {
    if size == 0 || size > 8 {
        return Err(PlistError("invalid plist integer width"));
    }
    let end = start
        .checked_add(size)
        .ok_or(PlistError("plist offset overflow"))?;
    let bytes = data
        .get(start..end)
        .ok_or(PlistError("truncated plist integer"))?;
    Ok(bytes
        .iter()
        .fold(0u64, |value, byte| (value << 8) | u64::from(*byte)))
}

fn write_uint(output: &mut Vec<u8>, value: u64, size: usize) {
    for index in (0..size).rev() {
        output.push((value >> (index * 8)) as u8);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn binary_plist_round_trip_covers_airdrop_values() {
        let value = Value::Dictionary(vec![
            (
                "SenderComputerName".into(),
                Value::String("Mac mini 本地".into()),
            ),
            ("Flags".into(), Value::Integer(0x1b3fb)),
            ("Payload".into(), Value::Data(vec![0, 1, 2, 255])),
            ("Accepted".into(), Value::Bool(true)),
            (
                "Items".into(),
                Value::Array(vec![Value::Null, Value::Integer(-5), Value::Real(1.25)]),
            ),
            ("Timestamp".into(), Value::Date(12345.5)),
            ("UID".into(), Value::Uid(0x1234)),
        ]);
        let bytes = write(&value).unwrap();
        assert_eq!(read(&bytes).unwrap(), value);
    }

    #[test]
    fn rejects_truncated_and_cyclic_objects() {
        assert!(read(b"bplist00").is_err());
        let value = Value::Array(vec![Value::String("ok".into())]);
        let bytes = write(&value).unwrap();
        let mut damaged = bytes;
        let start = damaged.len() - 24;
        damaged[start..start + 8].copy_from_slice(&99u64.to_be_bytes());
        assert!(read(&damaged).is_err());
    }
}
