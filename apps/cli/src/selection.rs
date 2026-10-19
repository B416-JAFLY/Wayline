use omdrop_core::NearbyReceiver;
use std::io::{self, BufRead, Write};

/// Explicit input is required even when exactly one receiver was found.
pub fn choose<'a>(
    receivers: &'a [NearbyReceiver],
    input: &mut impl BufRead,
    output: &mut impl Write,
) -> io::Result<Option<&'a NearbyReceiver>> {
    if receivers.is_empty() {
        return Err(io::Error::new(
            io::ErrorKind::NotFound,
            "no AirDrop receivers answered",
        ));
    }
    for (index, receiver) in receivers.iter().enumerate() {
        writeln!(
            output,
            "  [{}] {:?}  {}  ({:?})",
            index + 1,
            receiver.name,
            receiver.service.address,
            receiver.service.instance
        )?;
    }
    write!(output, "Choose a device number, or press Enter to cancel: ")?;
    output.flush()?;
    let mut answer = String::new();
    std::io::Read::take(&mut *input, 256).read_line(&mut answer)?;
    if answer.len() >= 256 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "device choice is too long",
        ));
    }
    let answer = answer.trim();
    if answer.is_empty() {
        return Ok(None);
    }
    let index = answer
        .parse::<usize>()
        .ok()
        .and_then(|number| number.checked_sub(1));
    match index.and_then(|index| receivers.get(index)) {
        Some(receiver) => Ok(Some(receiver)),
        None => Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "invalid device number; no files sent",
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use omdrop_core::AirDropService;
    #[test]
    fn duplicate_names_keep_distinct_choices_and_blank_eof_or_invalid_never_select() {
        let receivers: Vec<_> = (1..=2)
            .map(|i| NearbyReceiver {
                name: "iPhone".into(),
                service: AirDropService {
                    instance: format!("{i}._airdrop._tcp.local"),
                    host: format!("{i}.local"),
                    port: 8770,
                    address: format!("[fe80::{i}%1]:8770").parse().unwrap(),
                    flags: None,
                },
            })
            .collect();
        assert_eq!(
            choose(&receivers, &mut &b"2\n"[..], &mut Vec::new())
                .unwrap()
                .unwrap(),
            &receivers[1]
        );
        for input in [b"\n".as_slice(), b""] {
            assert!(choose(&receivers, &mut &input[..], &mut Vec::new())
                .unwrap()
                .is_none());
        }
        for input in ["0\n", "3\n", "1;send\n"] {
            assert!(choose(&receivers, &mut input.as_bytes(), &mut Vec::new()).is_err());
        }
        assert!(choose(&receivers[..1], &mut &b"\n"[..], &mut Vec::new())
            .unwrap()
            .is_none());
    }
}
