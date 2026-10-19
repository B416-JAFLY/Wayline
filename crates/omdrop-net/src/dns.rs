//! Small, bounds-checked DNS/mDNS codec.

use std::fmt;

pub const TYPE_A: u16 = 1;
pub const TYPE_PTR: u16 = 12;
pub const TYPE_TXT: u16 = 16;
pub const TYPE_AAAA: u16 = 28;
pub const TYPE_SRV: u16 = 33;
pub const TYPE_ANY: u16 = 255;
pub const CLASS_IN: u16 = 1;
pub const CLASS_UNICAST_RESPONSE: u16 = 0x8000;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Question {
    pub name: String,
    pub kind: u16,
    pub class: u16,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Record {
    pub name: String,
    pub kind: u16,
    pub class: u16,
    pub ttl: u32,
    pub data: Vec<u8>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RecordView {
    pub name: String,
    pub kind: u16,
    pub class: u16,
    pub ttl: u32,
    pub data_offset: usize,
    pub data_len: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Message {
    pub id: u16,
    pub flags: u16,
    pub questions: Vec<Question>,
    pub answers: Vec<Record>,
    pub authorities: Vec<Record>,
    pub additionals: Vec<Record>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Error {
    Truncated,
    InvalidName,
    PointerLoop,
    LabelTooLong,
    PacketTooLarge,
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Truncated => "truncated DNS packet",
            Self::InvalidName => "invalid DNS name",
            Self::PointerLoop => "DNS compression pointer loop",
            Self::LabelTooLong => "DNS label is too long",
            Self::PacketTooLarge => "DNS packet is too large",
        })
    }
}

impl std::error::Error for Error {}

pub fn encode_name(name: &str) -> Result<Vec<u8>, Error> {
    let mut out = Vec::new();
    for label in name.trim_end_matches('.').split('.') {
        if label.is_empty() || label.len() > 63 {
            return Err(Error::LabelTooLong);
        }
        if out.len() + label.len() + 2 > 255 {
            return Err(Error::PacketTooLarge);
        }
        out.push(label.len() as u8);
        out.extend_from_slice(label.as_bytes());
    }
    out.push(0);
    Ok(out)
}

pub fn read_name(packet: &[u8], offset: &mut usize) -> Result<String, Error> {
    let mut labels = Vec::new();
    let mut position = *offset;
    let mut jumped = false;
    let mut jumps = 0usize;
    loop {
        let length = *packet.get(position).ok_or(Error::Truncated)?;
        if length == 0 {
            position += 1;
            if !jumped {
                *offset = position;
            }
            return Ok(labels.join("."));
        }
        if length & 0xc0 == 0xc0 {
            let next = *packet.get(position + 1).ok_or(Error::Truncated)?;
            if !jumped {
                *offset = position + 2;
            }
            position = ((usize::from(length & 0x3f)) << 8) | usize::from(next);
            jumped = true;
            jumps += 1;
            if jumps > 32 {
                return Err(Error::PointerLoop);
            }
            continue;
        }
        if length & 0xc0 != 0 || length > 63 {
            return Err(Error::InvalidName);
        }
        let end = position
            .checked_add(1 + usize::from(length))
            .ok_or(Error::Truncated)?;
        let bytes = packet.get(position + 1..end).ok_or(Error::Truncated)?;
        labels.push(String::from_utf8_lossy(bytes).into_owned());
        position = end;
        if !jumped {
            *offset = position;
        }
    }
}

pub fn parse(packet: &[u8]) -> Result<Message, Error> {
    if packet.len() < 12 {
        return Err(Error::Truncated);
    }
    let id = u16::from_be_bytes([packet[0], packet[1]]);
    let flags = u16::from_be_bytes([packet[2], packet[3]]);
    let question_count = usize::from(u16::from_be_bytes([packet[4], packet[5]]));
    let answer_count = usize::from(u16::from_be_bytes([packet[6], packet[7]]));
    let authority_count = usize::from(u16::from_be_bytes([packet[8], packet[9]]));
    let additional_count = usize::from(u16::from_be_bytes([packet[10], packet[11]]));
    let mut offset = 12usize;
    let mut questions = Vec::with_capacity(question_count);
    for _ in 0..question_count {
        let name = read_name(packet, &mut offset)?;
        let kind = read_u16(packet, &mut offset)?;
        let class = read_u16(packet, &mut offset)?;
        questions.push(Question { name, kind, class });
    }
    let answers = read_records(packet, &mut offset, answer_count)?;
    let authorities = read_records(packet, &mut offset, authority_count)?;
    let additionals = read_records(packet, &mut offset, additional_count)?;
    Ok(Message {
        id,
        flags,
        questions,
        answers,
        authorities,
        additionals,
    })
}

pub fn parse_record_views(packet: &[u8]) -> Result<(Vec<Question>, Vec<RecordView>), Error> {
    if packet.len() < 12 {
        return Err(Error::Truncated);
    }
    let question_count = usize::from(u16::from_be_bytes([packet[4], packet[5]]));
    let answer_count = usize::from(u16::from_be_bytes([packet[6], packet[7]]));
    let authority_count = usize::from(u16::from_be_bytes([packet[8], packet[9]]));
    let additional_count = usize::from(u16::from_be_bytes([packet[10], packet[11]]));
    let mut offset = 12usize;
    let mut questions = Vec::with_capacity(question_count);
    for _ in 0..question_count {
        let name = read_name(packet, &mut offset)?;
        let kind = read_u16(packet, &mut offset)?;
        let class = read_u16(packet, &mut offset)?;
        questions.push(Question { name, kind, class });
    }
    let total = answer_count
        .checked_add(authority_count)
        .and_then(|value| value.checked_add(additional_count))
        .ok_or(Error::PacketTooLarge)?;
    Ok((questions, read_record_views(packet, &mut offset, total)?))
}

pub fn query(questions: &[Question]) -> Result<Vec<u8>, Error> {
    let mut packet = Vec::with_capacity(512);
    write_header(&mut packet, 0, 0, questions.len(), 0, 0, 0)?;
    for question in questions {
        packet.extend_from_slice(&encode_name(&question.name)?);
        packet.extend_from_slice(&question.kind.to_be_bytes());
        packet.extend_from_slice(&question.class.to_be_bytes());
    }
    Ok(packet)
}

pub fn response(records: &[Record]) -> Result<Vec<u8>, Error> {
    let mut packet = Vec::with_capacity(1024);
    write_header(&mut packet, 0, 0x8400, 0, records.len(), 0, 0)?;
    for record in records {
        write_record(&mut packet, record)?;
    }
    Ok(packet)
}

fn read_records(packet: &[u8], offset: &mut usize, count: usize) -> Result<Vec<Record>, Error> {
    let mut records = Vec::with_capacity(count);
    for _ in 0..count {
        let name = read_name(packet, offset)?;
        let kind = read_u16(packet, offset)?;
        let class = read_u16(packet, offset)?;
        let ttl = read_u32(packet, offset)?;
        let length = usize::from(read_u16(packet, offset)?);
        let end = offset.checked_add(length).ok_or(Error::Truncated)?;
        let data = packet.get(*offset..end).ok_or(Error::Truncated)?.to_vec();
        *offset = end;
        records.push(Record {
            name,
            kind,
            class,
            ttl,
            data,
        });
    }
    Ok(records)
}

fn read_record_views(
    packet: &[u8],
    offset: &mut usize,
    count: usize,
) -> Result<Vec<RecordView>, Error> {
    let mut records = Vec::with_capacity(count);
    for _ in 0..count {
        let name = read_name(packet, offset)?;
        let kind = read_u16(packet, offset)?;
        let class = read_u16(packet, offset)?;
        let ttl = read_u32(packet, offset)?;
        let length = usize::from(read_u16(packet, offset)?);
        let data_offset = *offset;
        let end = data_offset.checked_add(length).ok_or(Error::Truncated)?;
        if end > packet.len() {
            return Err(Error::Truncated);
        }
        *offset = end;
        records.push(RecordView {
            name,
            kind,
            class,
            ttl,
            data_offset,
            data_len: length,
        });
    }
    Ok(records)
}

fn write_header(
    packet: &mut Vec<u8>,
    id: u16,
    flags: u16,
    questions: usize,
    answers: usize,
    authorities: usize,
    additionals: usize,
) -> Result<(), Error> {
    if [questions, answers, authorities, additionals]
        .iter()
        .any(|&value| value > usize::from(u16::MAX))
    {
        return Err(Error::PacketTooLarge);
    }
    for value in [
        id,
        flags,
        questions as u16,
        answers as u16,
        authorities as u16,
        additionals as u16,
    ] {
        packet.extend_from_slice(&value.to_be_bytes());
    }
    Ok(())
}

fn write_record(packet: &mut Vec<u8>, record: &Record) -> Result<(), Error> {
    packet.extend_from_slice(&encode_name(&record.name)?);
    packet.extend_from_slice(&record.kind.to_be_bytes());
    packet.extend_from_slice(&record.class.to_be_bytes());
    packet.extend_from_slice(&record.ttl.to_be_bytes());
    let length = u16::try_from(record.data.len()).map_err(|_| Error::PacketTooLarge)?;
    packet.extend_from_slice(&length.to_be_bytes());
    packet.extend_from_slice(&record.data);
    Ok(())
}

fn read_u16(packet: &[u8], offset: &mut usize) -> Result<u16, Error> {
    let end = offset.checked_add(2).ok_or(Error::Truncated)?;
    let bytes = packet.get(*offset..end).ok_or(Error::Truncated)?;
    *offset = end;
    Ok(u16::from_be_bytes([bytes[0], bytes[1]]))
}

fn read_u32(packet: &[u8], offset: &mut usize) -> Result<u32, Error> {
    let end = offset.checked_add(4).ok_or(Error::Truncated)?;
    let bytes = packet.get(*offset..end).ok_or(Error::Truncated)?;
    *offset = end;
    Ok(u32::from_be_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn query_and_response_round_trip() {
        let questions = [Question {
            name: "_airdrop._tcp.local".into(),
            kind: TYPE_PTR,
            class: CLASS_IN,
        }];
        let query_packet = query(&questions).unwrap();
        assert_eq!(parse(&query_packet).unwrap().questions, questions);
        let records = [Record {
            name: "host.local".into(),
            kind: TYPE_TXT,
            class: CLASS_IN | CLASS_UNICAST_RESPONSE,
            ttl: 120,
            data: b"flags=137".to_vec(),
        }];
        let response_packet = response(&records).unwrap();
        assert_eq!(parse(&response_packet).unwrap().answers, records);
    }

    #[test]
    fn compressed_names_are_bounded() {
        let packet = [0, 0, 0, 0, 0, 1, 0, 0, 0, 0, 0, 0, 0xc0, 0x0c, 0, 12, 0, 1];
        assert_eq!(parse(&packet), Err(Error::PointerLoop));
    }
}
