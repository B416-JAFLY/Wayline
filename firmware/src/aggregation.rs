//! Bounded AWDL receive Block Ack negotiation. The HAL owns the hardware BA table.
//!
//! Only one peer/TID is admitted, with at most 16 MPDUs per window. Hardware sends
//! the time-critical Block Ack; neither the USB link nor this task sends it.
//! A-MSDU inside A-MPDU is declined to keep each forwarded MPDU within the radio
//! protocol's existing frame bound. TCP continues to validate/reassemble payloads.

pub const MAX_WINDOW: u8 = 16;
pub const CONTROL_MAX: usize = 64;
const BSSID: [u8; 6] = [0x00, 0x25, 0x00, 0xff, 0x94, 0x73];
const IDLE_MS: u32 = 30_000;
const DECLINED: u16 = 37;

pub trait Hardware {
    /// Install the hardware receive agreement before accepting it on the air.
    fn start(&mut self, peer: &[u8; 6], tid: u8, ssn: u16, window: u8) -> bool;
    fn stop(&mut self);
    /// Enqueue a management frame for transmission, never a software Block Ack.
    fn transmit(&mut self, frame: &[u8]) -> bool;
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Counters {
    pub requests: u32,
    pub accepted: u32,
    pub declined: u32,
    pub response_errors: u32,
    pub ended: u32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Session {
    peer: [u8; 6],
    tid: u8,
    token: u8,
    ssn: u16,
    window: u8,
    timeout: u16,
    last_rx_ms: u32,
}

pub struct Receiver {
    session: Option<Session>,
    sequence: u16,
    pub counters: Counters,
}

impl Default for Receiver {
    fn default() -> Self {
        Self::new()
    }
}

impl Receiver {
    pub const fn new() -> Self {
        Self {
            session: None,
            sequence: 0,
            counters: Counters {
                requests: 0,
                accepted: 0,
                declined: 0,
                response_errors: 0,
                ended: 0,
            },
        }
    }

    pub fn active(&self) -> Option<(u8, u8)> {
        self.session.map(|s| (s.tid, s.window))
    }

    /// Called with the radio configuration mutex, before a MAC/channel change.
    pub fn reset(&mut self, hw: &mut impl Hardware) {
        if self.session.take().is_some() {
            hw.stop();
            self.counters.ended = self.counters.ended.wrapping_add(1);
        }
    }

    pub fn tick(&mut self, own: &[u8; 6], now_ms: u32, hw: &mut impl Hardware) {
        let Some(s) = self.session else { return };
        // Even a zero-timeout agreement may be explicitly torn down with DELBA.
        let limit = if s.timeout == 0 {
            IDLE_MS
        } else {
            (u32::from(s.timeout) * 1024).div_ceil(1000)
        };
        if now_ms.wrapping_sub(s.last_rx_ms) >= limit {
            let mut out = self.action_header(own, &s.peer);
            out[25] = 2; // DELBA, recipient of the data (initiator bit clear).
            out[26..28].copy_from_slice(&(u16::from(s.tid) << 12).to_le_bytes());
            out[28..30].copy_from_slice(&39u16.to_le_bytes()); // timeout
            self.transmit(&out[..30], hw);
            self.reset(hw);
        }
    }

    /// Track matching QoS data/BAR activity without allocating or calling drivers.
    pub fn observe(&mut self, frame: &[u8], own: &[u8; 6], now_ms: u32) {
        let Some(s) = self.session.as_mut() else {
            return;
        };
        if frame.len() < 20 || frame[4..10] != *own || frame[10..16] != s.peer {
            return;
        }
        let tid = if frame[0] == 0x88
            && frame.len() >= 26
            && frame[1] & 0xc7 == 0
            && frame[16..22] == BSSID
        {
            frame[24] & 15
        } else if frame[0] == 0x84 && frame[1] == 0 && frame[16] & 2 == 0 {
            frame[17] >> 4 // single-TID BAR
        } else {
            return;
        };
        if tid == s.tid {
            s.last_rx_ms = now_ms;
        }
    }

    pub fn action(&mut self, frame: &[u8], own: &[u8; 6], now_ms: u32, hw: &mut impl Hardware) {
        if !is_action(frame, own) {
            return;
        }
        let peer: [u8; 6] = frame[10..16].try_into().unwrap();
        match frame[25] {
            0 if frame.len() >= 33 => {
                self.counters.requests = self.counters.requests.wrapping_add(1);
                let params = u16::from_le_bytes([frame[27], frame[28]]);
                let timeout = u16::from_le_bytes([frame[29], frame[30]]);
                let sequence = u16::from_le_bytes([frame[31], frame[32]]);
                let tid = ((params >> 2) & 15) as u8;
                let requested = params >> 6;
                let window = if requested == 0 {
                    MAX_WINDOW
                } else {
                    requested.min(MAX_WINDOW.into()) as u8
                };
                let candidate = Session {
                    peer,
                    tid,
                    token: frame[26],
                    ssn: sequence >> 4,
                    window,
                    timeout,
                    last_rx_ms: now_ms,
                };
                // Extended TIDs, delayed BA and fragmented MPDUs are unsupported.
                let valid = params & 2 != 0 && tid < 8 && window >= 2 && sequence & 15 == 0;
                let duplicate = self.session.is_some_and(|s| {
                    Session {
                        last_rx_ms: now_ms,
                        ..s
                    } == candidate
                });
                let busy = self.session.is_some_and(|s| s.peer != peer || s.tid != tid);
                if !valid || busy {
                    self.response(own, &candidate, DECLINED, hw);
                    return;
                }
                if duplicate {
                    // A retry must not rewind the hardware bitmap/starting sequence.
                    self.response(own, &candidate, 0, hw);
                    return;
                }
                self.reset(hw);
                if !hw.start(&peer, tid, candidate.ssn, window) {
                    // start() must leave no active hardware agreement on failure.
                    self.response(own, &candidate, DECLINED, hw);
                    return;
                }
                if self.response(own, &candidate, 0, hw) {
                    self.session = Some(candidate);
                } else {
                    hw.stop(); // never keep a new agreement after an enqueue failure
                }
            }
            2 if frame.len() >= 30 => {
                let params = u16::from_le_bytes([frame[26], frame[27]]);
                if params & 0x0800 != 0
                    && self
                        .session
                        .is_some_and(|s| s.peer == peer && u16::from(s.tid) == params >> 12)
                {
                    self.reset(hw);
                }
            }
            _ => (),
        }
    }

    fn action_header(&mut self, own: &[u8; 6], peer: &[u8; 6]) -> [u8; 33] {
        let mut out = [0u8; 33];
        out[0] = 0xd0;
        out[4..10].copy_from_slice(peer);
        out[10..16].copy_from_slice(own);
        out[16..22].copy_from_slice(&BSSID);
        out[22..24].copy_from_slice(&(self.sequence << 4).to_le_bytes());
        self.sequence = self.sequence.wrapping_add(1) & 0x0fff;
        out[24] = 3;
        out
    }

    fn response(
        &mut self,
        own: &[u8; 6],
        s: &Session,
        status: u16,
        hw: &mut impl Hardware,
    ) -> bool {
        let mut out = self.action_header(own, &s.peer);
        out[25] = 1;
        out[26] = s.token;
        out[27..29].copy_from_slice(&status.to_le_bytes());
        // Clear A-MSDU support; preserve TID, negotiate immediate BA and the bound.
        let window = if status == 0 { s.window } else { 0 };
        let params = 2 | (u16::from(s.tid) << 2) | (u16::from(window) << 6);
        out[29..31].copy_from_slice(&params.to_le_bytes());
        out[31..33].copy_from_slice(&s.timeout.to_le_bytes());
        let sent = self.transmit(&out, hw);
        if sent {
            if status == 0 {
                self.counters.accepted = self.counters.accepted.wrapping_add(1);
            } else {
                self.counters.declined = self.counters.declined.wrapping_add(1);
            }
        }
        sent
    }

    fn transmit(&mut self, frame: &[u8], hw: &mut impl Hardware) -> bool {
        if hw.transmit(frame) {
            true
        } else {
            self.counters.response_errors = self.counters.response_errors.wrapping_add(1);
            false
        }
    }
}

/// Only unprotected, unfragmented Block Ack actions addressed to this AWDL radio.
pub fn is_action(frame: &[u8], own: &[u8; 6]) -> bool {
    frame.len() >= 26
        && frame.len() <= CONTROL_MAX
        && frame[0] == 0xd0
        && frame[1] & 0xc7 == 0
        && frame[22] & 15 == 0
        && frame[4..10] == *own
        && frame[10] & 1 == 0
        && frame[10..16] != [0; 6]
        && frame[10..16] != *own
        && frame[16..22] == BSSID
        && frame[24] == 3
        && matches!(frame[25], 0 | 2)
}

#[cfg(test)]
mod tests {
    use super::*;
    const OWN: [u8; 6] = [2, 1, 2, 3, 4, 5];
    const PEER: [u8; 6] = [2, 6, 7, 8, 9, 10];

    #[derive(Default)]
    struct Radio {
        fail_start: bool,
        fail_tx: bool,
        starts: Vec<([u8; 6], u8, u16, u8)>,
        stops: usize,
        frames: Vec<Vec<u8>>,
    }
    impl Hardware for Radio {
        fn start(&mut self, peer: &[u8; 6], tid: u8, ssn: u16, window: u8) -> bool {
            self.starts.push((*peer, tid, ssn, window));
            !self.fail_start
        }
        fn stop(&mut self) {
            self.stops += 1;
        }
        fn transmit(&mut self, frame: &[u8]) -> bool {
            self.frames.push(frame.to_vec());
            !self.fail_tx
        }
    }
    fn request(tid: u8, window: u16, timeout: u16) -> [u8; 33] {
        let mut f = Receiver::new().action_header(&PEER, &OWN);
        f[25] = 0;
        f[26] = 19;
        f[27..29].copy_from_slice(&(3 | (u16::from(tid) << 2) | (window << 6)).to_le_bytes());
        f[29..31].copy_from_slice(&timeout.to_le_bytes());
        f[31..33].copy_from_slice(&0xfff0u16.to_le_bytes());
        f
    }

    #[test]
    fn accepts_bounded_window_only_after_hardware_setup() {
        for (asked, want) in [(0, 16), (2, 2), (8, 8), (64, 16), (1023, 16)] {
            let (mut rx, mut hw) = (Receiver::new(), Radio::default());
            rx.action(&request(5, asked, 123), &OWN, 9, &mut hw);
            assert_eq!(hw.starts, [(PEER, 5, 4095, want)]);
            assert_eq!(rx.active(), Some((5, want)));
            let f = &hw.frames[0];
            assert_eq!(&f[4..10], &PEER);
            assert_eq!(&f[10..16], &OWN);
            assert_eq!(&f[16..22], &BSSID);
            assert_eq!(&f[24..29], &[3, 1, 19, 0, 0]);
            assert_eq!(
                u16::from_le_bytes([f[29], f[30]]),
                2 | (5 << 2) | (u16::from(want) << 6)
            );
            assert_eq!(&f[31..33], &123u16.to_le_bytes());
        }
    }

    #[test]
    fn retry_does_not_reset_bitmap_and_new_token_replaces_agreement() {
        let (mut rx, mut hw) = (Receiver::new(), Radio::default());
        let mut f = request(5, 64, 0);
        rx.action(&f, &OWN, 0, &mut hw);
        f[1] |= 8; // retry
        rx.action(&f, &OWN, 1, &mut hw);
        assert_eq!(hw.starts.len(), 1);
        assert_eq!(hw.stops, 0);
        assert_eq!(hw.frames.len(), 2);
        f[26] += 1;
        rx.action(&f, &OWN, 2, &mut hw);
        assert_eq!(hw.starts.len(), 2);
        assert_eq!(hw.stops, 1);
    }

    #[test]
    fn failures_decline_or_release_hardware_without_false_active_state() {
        let (mut rx, mut hw) = (
            Receiver::new(),
            Radio {
                fail_start: true,
                ..Radio::default()
            },
        );
        rx.action(&request(5, 64, 0), &OWN, 0, &mut hw);
        assert_eq!(rx.active(), None);
        assert_eq!(&hw.frames[0][27..29], &DECLINED.to_le_bytes());
        hw.fail_start = false;
        hw.fail_tx = true;
        rx.action(&request(5, 64, 0), &OWN, 0, &mut hw);
        assert_eq!(rx.active(), None);
        assert_eq!(hw.stops, 1);
        assert_eq!(rx.counters.response_errors, 1);
    }

    #[test]
    fn malformed_other_peer_and_unsupported_requests_cannot_install_sessions() {
        let f = request(5, 64, 0);
        let (mut rx, mut hw) = (Receiver::new(), Radio::default());
        for len in 0..33 {
            rx.action(&f[..len], &OWN, 0, &mut hw);
        }
        for (index, value) in [
            (0, 0x88),
            (1, 0x40),
            (1, 4),
            (1, 0x80),
            (4, 4),
            (10, 3),
            (16, 1),
            (22, 1),
        ] {
            let mut bad = f;
            bad[index] = value;
            rx.action(&bad, &OWN, 0, &mut hw);
        }
        assert!(hw.starts.is_empty());
        assert!(hw.frames.is_empty());
        let mut delayed = f;
        delayed[27] &= !2;
        for bad in [request(8, 64, 0), request(5, 1, 0), delayed] {
            rx.action(&bad, &OWN, 0, &mut hw);
        }
        let mut fragmented = f;
        fragmented[31] |= 1;
        rx.action(&fragmented, &OWN, 0, &mut hw);
        assert!(hw.starts.is_empty());
        assert!(hw.frames.iter().all(|f| f[27] == DECLINED as u8));
        rx.action(&f, &OWN, 0, &mut hw);
        rx.action(&request(4, 64, 0), &OWN, 1, &mut hw);
        let mut other = f;
        other[15] += 1;
        rx.action(&other, &OWN, 1, &mut hw);
        assert_eq!(hw.starts.len(), 1);
        assert_eq!(hw.stops, 0);
    }

    #[test]
    fn delba_direction_and_tid_are_checked() {
        let (mut rx, mut hw) = (Receiver::new(), Radio::default());
        rx.action(&request(5, 64, 0), &OWN, 0, &mut hw);
        let mut f = Receiver::new().action_header(&PEER, &OWN);
        f[25] = 2;
        for params in [5u16 << 12, (4 << 12) | 0x800] {
            f[26..28].copy_from_slice(&params.to_le_bytes());
            rx.action(&f[..30], &OWN, 0, &mut hw);
            assert!(rx.active().is_some());
        }
        f[26..28].copy_from_slice(&((5u16 << 12) | 0x800).to_le_bytes());
        rx.action(&f[..30], &OWN, 0, &mut hw);
        assert_eq!(rx.active(), None);
        rx.reset(&mut hw);
        assert_eq!(hw.stops, 1);
    }

    #[test]
    fn activity_and_timeout_handle_clock_wrap_and_send_delba() {
        let (mut rx, mut hw) = (Receiver::new(), Radio::default());
        rx.action(&request(5, 64, 10), &OWN, u32::MAX - 5, &mut hw);
        rx.tick(&OWN, 4, &mut hw); // elapsed 10, timeout is ceil(10.24) ms
        assert!(rx.active().is_some());
        let mut data = request(5, 64, 0);
        data[0] = 0x88;
        data[24] = 5;
        rx.observe(&data, &OWN, 4);
        rx.tick(&OWN, 14, &mut hw);
        assert!(rx.active().is_some());
        data[24] = 4;
        rx.observe(&data, &OWN, 14); // other TID cannot keep it alive
        rx.tick(&OWN, 15, &mut hw);
        assert_eq!(rx.active(), None);
        assert_eq!(hw.frames.last().unwrap().len(), 30);
        assert_eq!(&hw.frames.last().unwrap()[24..30], &[3, 2, 0, 0x50, 39, 0]);
    }

    #[test]
    fn bar_keeps_session_alive_and_zero_timeout_is_explicitly_torn_down() {
        let (mut rx, mut hw) = (Receiver::new(), Radio::default());
        rx.action(&request(5, 64, 0), &OWN, 0, &mut hw);
        let mut bar = [0u8; 20];
        bar[0] = 0x84;
        bar[4..10].copy_from_slice(&OWN);
        bar[10..16].copy_from_slice(&PEER);
        bar[16] = 4; // compressed, single-TID BAR
        bar[17] = 0x50;
        rx.observe(&bar, &OWN, IDLE_MS - 1);
        rx.tick(&OWN, IDLE_MS, &mut hw);
        assert!(rx.active().is_some());
        // Multi-TID BAR and a BAR from another sender must not refresh it.
        bar[16] |= 2;
        rx.observe(&bar, &OWN, IDLE_MS * 2 - 2);
        bar[16] &= !2;
        bar[15] ^= 1;
        rx.observe(&bar, &OWN, IDLE_MS * 2 - 2);
        rx.tick(&OWN, IDLE_MS * 2 - 1, &mut hw);
        assert_eq!(rx.active(), None);
        assert_eq!(hw.stops, 1);
        assert_eq!(hw.frames.last().unwrap()[25], 2);
    }

    #[test]
    fn failed_retry_response_keeps_an_established_agreement() {
        let (mut rx, mut hw) = (Receiver::new(), Radio::default());
        let f = request(5, 64, 0);
        rx.action(&f, &OWN, 0, &mut hw);
        hw.fail_tx = true;
        rx.action(&f, &OWN, 1, &mut hw);
        assert_eq!(rx.active(), Some((5, 16)));
        assert_eq!(hw.starts.len(), 1);
        assert_eq!(hw.stops, 0);
        assert_eq!(rx.counters.response_errors, 1);
    }
}
