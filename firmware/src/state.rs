//! Radio filtering, counters, peer tracking and keyboard policy, independent of drivers.

use omdrop_proto::FilterMode;

pub const PEER_MAX: usize = 4;
const APPLE_OUI: &[u8] = &[0x00, 0x17, 0xf2];
const AWDL_BSSID: &[u8] = &[0x00, 0x25, 0x00, 0xff, 0x94, 0x73];

#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Stats {
    pub rx: u32,
    pub rx_drop: u32,
    pub tx_ok: u32,
    pub tx_err: u32,
    pub tx_acked: u32,
    pub tx_noack: u32,
    pub bad_msg: u32,
}

impl Stats {
    pub fn wire_bytes(&self) -> [u8; 28] {
        let mut out = [0; 28];
        let counters = [
            self.rx,
            self.rx_drop,
            self.tx_ok,
            self.tx_err,
            self.tx_acked,
            self.tx_noack,
            self.bad_msg,
        ];
        for (bytes, value) in out.as_chunks_mut::<4>().0.iter_mut().zip(counters) {
            bytes.copy_from_slice(&value.to_le_bytes());
        }
        out
    }
}

#[repr(C)]
#[derive(Clone, Copy, Debug, Default)]
pub struct Peer {
    pub mac: [u8; 6],
    pub rssi: i8,
    pub psf: u16,
    pub mif: u16,
    pub last_seen_ms: u32,
}

#[derive(Clone, Copy)]
pub struct RadioState {
    pub feedback: crate::feedback::Feedback,
    pub mac: [u8; 6],
    pub channel: u8,
    pub filter: FilterMode,
    pub stats: Stats,
    pub uplink: crate::uplink::Stats,
    pub commands: u32,
    pub hellos: u32,
    pub last_command_ms: Option<u32>,
    pub peers: [Peer; PEER_MAX],
    pub peer_count: usize,
}

impl Default for RadioState {
    fn default() -> Self {
        Self::new()
    }
}

impl RadioState {
    pub const fn new() -> Self {
        Self {
            feedback: crate::feedback::Feedback::new(),
            mac: [0; 6],
            channel: 6,
            filter: FilterMode::Awdl,
            stats: Stats {
                rx: 0,
                rx_drop: 0,
                tx_ok: 0,
                tx_err: 0,
                tx_acked: 0,
                tx_noack: 0,
                bad_msg: 0,
            },
            uplink: crate::uplink::Stats::new(),
            commands: 0,
            hellos: 0,
            last_command_ms: None,
            peers: [Peer {
                mac: [0; 6],
                rssi: 0,
                psf: 0,
                mif: 0,
                last_seen_ms: 0,
            }; PEER_MAX],
            peer_count: 0,
        }
    }

    pub fn record_command(&mut self, kind: u8, now_ms: u32) {
        self.last_command_ms = Some(now_ms);
        self.commands = self.commands.wrapping_add(1);
        if kind == 1 {
            self.hellos = self.hellos.wrapping_add(1);
        }
    }

    pub fn host_active(&self, now_ms: u32) -> bool {
        self.last_command_ms
            .is_some_and(|last| now_ms.wrapping_sub(last) < 5000)
    }

    pub fn record_peer(&mut self, frame: &[u8], rssi: i8, now_ms: u32) {
        if frame.len() < 31 || !apple_action(frame) || frame[28] != 0x08 {
            return;
        }
        let mac: [u8; 6] = frame[10..16].try_into().unwrap();
        let index = self.peers[..self.peer_count]
            .iter()
            .position(|peer| peer.mac == mac)
            .unwrap_or_else(|| {
                let slot = if self.peer_count < PEER_MAX {
                    let slot = self.peer_count;
                    self.peer_count += 1;
                    slot
                } else {
                    (0..PEER_MAX)
                        .max_by_key(|&i| now_ms.wrapping_sub(self.peers[i].last_seen_ms))
                        .unwrap()
                };
                self.peers[slot] = Peer {
                    mac,
                    ..Peer::default()
                };
                slot
            });
        let peer = &mut self.peers[index];
        peer.rssi = rssi;
        peer.last_seen_ms = now_ms;
        match frame[30] {
            0 => peer.psf = peer.psf.wrapping_add(1),
            3 => peer.mif = peer.mif.wrapping_add(1),
            _ => (),
        }
    }
}

#[repr(u8)]
#[derive(Clone, Copy)]
pub enum PacketKind {
    Management = 0,
    Data = 1,
    Control = 2,
}

fn apple_action(frame: &[u8]) -> bool {
    frame.len() > 28 && frame[0] == 0xd0 && frame[24] == 127 && &frame[25..28] == APPLE_OUI
}

pub fn wanted(filter: FilterMode, kind: PacketKind, frame: &[u8], mac: &[u8; 6]) -> bool {
    match filter {
        FilterMode::All => true,
        FilterMode::ManagementAndData => !matches!(kind, PacketKind::Control),
        FilterMode::Awdl if frame.len() >= 24 => match kind {
            PacketKind::Management => apple_action(frame),
            PacketKind::Data => &frame[16..22] == AWDL_BSSID || &frame[4..10] == mac,
            PacketKind::Control => false,
        },
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn awdl_filter_only_accepts_apple_actions_bssid_or_our_destination() {
        let mac = [1, 2, 3, 4, 5, 6];
        let mut frame = [0; 32];
        assert!(!wanted(FilterMode::Awdl, PacketKind::Data, &frame, &mac));
        frame[4..10].copy_from_slice(&mac);
        assert!(wanted(FilterMode::Awdl, PacketKind::Data, &frame, &mac));
        assert!(!wanted(
            FilterMode::Awdl,
            PacketKind::Data,
            &frame[..23],
            &mac
        ));
        frame[4..10].fill(0);
        frame[16..22].copy_from_slice(AWDL_BSSID);
        assert!(wanted(FilterMode::Awdl, PacketKind::Data, &frame, &mac));
        frame[0] = 0xd0;
        frame[24] = 127;
        frame[25..28].copy_from_slice(APPLE_OUI);
        assert!(wanted(
            FilterMode::Awdl,
            PacketKind::Management,
            &frame,
            &mac
        ));
        frame[27] ^= 1;
        assert!(!wanted(
            FilterMode::Awdl,
            PacketKind::Management,
            &frame,
            &mac
        ));
        assert!(!wanted(
            FilterMode::ManagementAndData,
            PacketKind::Control,
            &frame,
            &mac
        ));
        assert!(wanted(FilterMode::All, PacketKind::Control, &frame, &mac));
    }

    #[test]
    fn host_activity_survives_timer_wrap() {
        let mut radio = RadioState::new();
        assert!(!radio.host_active(0));
        radio.record_command(1, u32::MAX - 100);
        assert!(radio.host_active(100));
    }

    #[test]
    fn peer_eviction_uses_elapsed_time_across_wrap_and_updates_counters() {
        let mut radio = RadioState::new();
        let mut frame = [0; 31];
        frame[0] = 0xd0;
        frame[24] = 127;
        frame[25..28].copy_from_slice(APPLE_OUI);
        frame[28] = 8;
        for i in 0..4 {
            frame[10] = i;
            radio.record_peer(&frame, -42, (u32::MAX - 2).wrapping_add(i.into()));
        }
        frame[10] = 9;
        radio.record_peer(&frame, -60, 3);
        assert_eq!(radio.peer_count, 4);
        assert!(!radio.peers.iter().any(|p| p.mac[0] == 0));
        frame[30] = 3;
        radio.record_peer(&frame, -50, 4);
        let peer = radio.peers.iter().find(|p| p.mac[0] == 9).unwrap();
        assert_eq!((peer.psf, peer.mif, peer.rssi), (1, 1, -50));
    }

    #[test]
    fn ui_abi_layout_and_stats_wire_order_are_stable() {
        assert_eq!(core::mem::size_of::<Stats>(), 28);
        assert_eq!(core::mem::size_of::<Peer>(), 16);
        assert_eq!(core::mem::offset_of!(Peer, psf), 8);
        assert_eq!(core::mem::offset_of!(Peer, last_seen_ms), 12);
        let stats = Stats {
            rx: 1,
            rx_drop: 2,
            tx_ok: 3,
            tx_err: 4,
            tx_acked: 5,
            tx_noack: 6,
            bad_msg: 7,
        };
        for (bytes, expected) in stats.wire_bytes().as_chunks::<4>().0.iter().zip(1u32..=7) {
            assert_eq!(*bytes, expected.to_le_bytes());
        }
    }
}
