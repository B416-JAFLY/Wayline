//! Protocol-v2 radio messages; independent of Windows and serial drivers.

use crate::{driver::Received, invalid};
use omdrop_proto::{HostMessage, PROTO_MAX_FRAME, RATE_MCS, RATE_MCS_SGI};
use owly_awdl::{Frame, TxRate, BSSID};
use std::{
    io,
    time::{Duration, Instant},
};

/// Maps the radio's wrapping microsecond counter to the host monotonic clock.
/// The least delayed sample bounds the offset; 1 µs/sample allows clock drift.
#[derive(Default)]
pub struct ReceiveClock {
    last: Option<u32>,
    extended: u64,
    base: Option<Instant>,
    offset: i128,
}

impl ReceiveClock {
    pub fn map(&mut self, timestamp: u32, arrived: Instant) -> io::Result<Instant> {
        if let Some(last) = self.last {
            let delta = timestamp.wrapping_sub(last);
            // A backward jump other than a normal wrap means a reset/reordered
            // stream. Reconnect instead of treating a device reset as 71 minutes.
            if delta > i32::MAX as u32 {
                return Err(invalid(
                    "radio timestamp moved backwards; reconnect the radio",
                ));
            }
            self.extended += u64::from(delta);
        }
        self.last = Some(timestamp);
        let base = *self.base.get_or_insert(arrived);
        let sample =
            arrived.saturating_duration_since(base).as_micros() as i128 - i128::from(self.extended);
        self.offset = sample.min(self.offset + 1);
        let micros = (i128::from(self.extended) + self.offset).max(0) as u64;
        Ok((base + Duration::from_micros(micros)).min(arrived))
    }
}

pub fn received(
    body: &[u8],
    channel: u8,
    arrived: Instant,
    clock: &mut ReceiveClock,
) -> io::Result<Option<Received>> {
    if !(10 + 24..=10 + PROTO_MAX_FRAME).contains(&body.len()) {
        return Ok(None);
    }
    if body[5] != channel || body[26..32] != BSSID.0 {
        return Ok(None);
    }
    let timestamp = u32::from_le_bytes(body[..4].try_into().unwrap());
    Ok(Some(Received {
        bytes: body[10..].to_vec(),
        rssi: Some(body[4] as i8),
        received_at: Some(clock.map(timestamp, arrived)?),
    }))
}

pub fn transmit(frame: &Frame) -> io::Result<Vec<u8>> {
    if !(24..=PROTO_MAX_FRAME).contains(&frame.bytes.len()) || frame.no_ack {
        return Err(invalid("unsupported ESP32 frame length or no-ACK flag"));
    }
    let rate = match frame.rate {
        TxRate::Legacy(r) if [2, 4, 11, 22, 12, 18, 24, 36, 48, 72, 96, 108].contains(&r) => r,
        TxRate::Ht { mcs, sgi } if mcs <= 7 => RATE_MCS | if sgi { RATE_MCS_SGI } else { 0 } | mcs,
        _ => return Err(invalid("unsupported ESP32 transmit rate")),
    };
    let mut msg = Vec::with_capacity(frame.bytes.len() + 5);
    msg.extend_from_slice(&[HostMessage::Tx as u8, rate, 1, 0, 0]);
    msg.extend_from_slice(&frame.bytes);
    Ok(msg)
}

#[cfg(test)]
mod tests {
    use super::*;
    use owly_awdl::MacAddr;

    #[test]
    fn timestamp_wrap_delay_drift_and_reset() {
        let t = Instant::now();
        let mut c = ReceiveClock::default();
        assert_eq!(c.map(u32::MAX - 9, t).unwrap(), t);
        assert_eq!(
            c.map(10, t + Duration::from_micros(1020)).unwrap(),
            t + Duration::from_micros(21)
        );
        assert_eq!(
            c.map(30, t + Duration::from_micros(40)).unwrap(),
            t + Duration::from_micros(40)
        );
        assert!(c.map(20, t + Duration::from_micros(50)).is_err());
    }

    #[test]
    fn receive_validates_frame_boundaries_channel_and_bssid() {
        let mut body = vec![0; 10 + PROTO_MAX_FRAME];
        body[4] = (-40i8) as u8;
        body[5] = 6;
        body[26..32].copy_from_slice(&BSSID.0);
        let mut clock = ReceiveClock::default();
        let now = Instant::now();
        assert_eq!(
            received(&body, 6, now, &mut clock)
                .unwrap()
                .unwrap()
                .bytes
                .len(),
            PROTO_MAX_FRAME
        );
        assert!(received(&body, 44, now, &mut clock).unwrap().is_none());
        assert!(received(&body[..33], 6, now, &mut clock).unwrap().is_none());
        body.push(0);
        assert!(received(&body, 6, now, &mut clock).unwrap().is_none());
    }

    #[test]
    fn tx_encodes_rates_and_rejects_unsupported_frames() {
        let mut frame = Frame {
            bytes: vec![0; 1600],
            to: MacAddr::BROADCAST,
            rate: TxRate::Ht { mcs: 7, sgi: true },
            no_ack: false,
        };
        let msg = transmit(&frame).unwrap();
        assert_eq!(&msg[..5], &[5, 0x97, 1, 0, 0]);
        frame.bytes.push(0);
        assert!(transmit(&frame).is_err());
        frame.bytes.pop();
        frame.rate = TxRate::Ht { mcs: 8, sgi: false };
        assert!(transmit(&frame).is_err());
        frame.rate = TxRate::BASIC;
        frame.no_ack = true;
        assert!(transmit(&frame).is_err());
    }
}
