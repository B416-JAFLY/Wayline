//! Layer-3 Wintun addressing and bounded neighbour discovery responses.
use owly_awdl::{Ipv6Packet, MacAddr};
use std::{
    collections::{BTreeMap, VecDeque},
    net::Ipv6Addr,
};

pub(crate) struct Network {
    mac: MacAddr,
    neighbours: BTreeMap<Ipv6Addr, MacAddr>,
    pub replies: VecDeque<Ipv6Packet>,
}

impl Network {
    pub fn new(mac: MacAddr) -> Self {
        Self {
            mac,
            neighbours: BTreeMap::new(),
            replies: VecDeque::new(),
        }
    }
    pub fn peer_up(&mut self, mac: MacAddr) {
        self.learn(mac.link_local(), mac);
    }
    pub fn peer_down(&mut self, mac: MacAddr) {
        self.neighbours.retain(|_, value| *value != mac);
    }
    fn learn(&mut self, addr: Ipv6Addr, mac: MacAddr) {
        if !addr.is_unicast_link_local() {
            return;
        }
        if self.neighbours.len() >= 512 && !self.neighbours.contains_key(&addr) {
            return;
        }
        self.neighbours.insert(addr, mac);
    }
    pub fn outbound(&self, bytes: &[u8]) -> Option<Ipv6Packet> {
        let data = ipv6(bytes)?;
        let dst = Ipv6Addr::from(<[u8; 16]>::try_from(&data[24..40]).ok()?);
        let dst = if dst.is_multicast() {
            let b = dst.octets();
            MacAddr([0x33, 0x33, b[12], b[13], b[14], b[15]])
        } else {
            self.neighbours
                .get(&dst)
                .copied()
                .or_else(|| MacAddr::from_link_local(dst))?
        };
        Some(Ipv6Packet {
            dst,
            src: self.mac,
            data: data.to_vec(),
        })
    }
    pub fn inbound(&mut self, packet: Ipv6Packet) -> Option<Vec<u8>> {
        let data = ipv6(&packet.data)?;
        let src = Ipv6Addr::from(<[u8; 16]>::try_from(&data[8..24]).ok()?);
        self.learn(src, packet.src);
        // Wintun is layer 3: answer NDP ourselves, as an Ethernet host would.
        if data[6] == 58
            && data.len() >= 64
            && data[40] == 135
            && data[48..64] == self.mac.link_local().octets()
        {
            if data[7] != 255 || data[41] != 0 || checksum(data) != 0 {
                return None;
            }
            if self.replies.len() >= 16 {
                return None;
            }
            let unspecified = src.is_unspecified();
            let dest = if unspecified {
                Ipv6Addr::new(0xff02, 0, 0, 0, 0, 0, 0, 1)
            } else {
                src
            };
            let mut reply = vec![0u8; 72];
            reply[0] = 0x60;
            reply[5] = 32;
            reply[6] = 58;
            reply[7] = 255;
            reply[8..24].copy_from_slice(&self.mac.link_local().octets());
            reply[24..40].copy_from_slice(&dest.octets());
            reply[40] = 136;
            reply[44] = if unspecified { 0x20 } else { 0x60 };
            reply[48..64].copy_from_slice(&self.mac.link_local().octets());
            reply[64..66].copy_from_slice(&[2, 1]);
            reply[66..72].copy_from_slice(&self.mac.0);
            let sum = checksum(&reply);
            reply[42..44].copy_from_slice(&sum.to_be_bytes());
            let dst = if unspecified {
                MacAddr([0x33, 0x33, 0, 0, 0, 1])
            } else {
                packet.src
            };
            self.replies.push_back(Ipv6Packet {
                src: self.mac,
                dst,
                data: reply,
            });
            return None;
        }
        Some(data.to_vec())
    }
}

fn ipv6(data: &[u8]) -> Option<&[u8]> {
    if data.len() < 40 || data[0] >> 4 != 6 {
        return None;
    }
    let len = 40 + usize::from(u16::from_be_bytes([data[4], data[5]]));
    if len > crate::MTU as usize {
        return None;
    }
    data.get(..len)
}

fn checksum(ip: &[u8]) -> u16 {
    let sum_bytes = |b: &[u8]| {
        b.chunks(2)
            .map(|p| u32::from(p[0]) * 256 + u32::from(*p.get(1).unwrap_or(&0)))
            .sum::<u32>()
    };
    let mut sum = sum_bytes(&ip[8..40]) + (ip.len() - 40) as u32 + 58 + sum_bytes(&ip[40..]);
    while sum >> 16 != 0 {
        sum = (sum & 0xffff) + (sum >> 16);
    }
    !(sum as u16)
}

#[cfg(test)]
mod tests {
    use super::*;
    const US: MacAddr = MacAddr([2, 0, 0, 0, 0, 1]);
    const PEER: MacAddr = MacAddr([2, 0, 0, 0, 0, 2]);
    #[test]
    fn packet_length_and_addresses_are_validated() {
        let mut net = Network::new(US);
        net.peer_up(PEER);
        let p = Ipv6Packet::echo_request(US, PEER, 1, 0, b"hello");
        assert_eq!(net.outbound(&p.data), Some(p.clone()));
        assert!(net.outbound(&p.data[..p.data.len() - 1]).is_none());
        let mut padded = p.data.clone();
        padded.extend([1, 2, 3]);
        assert_eq!(net.outbound(&padded), Some(p));
        net.peer_down(PEER);
    }
    #[test]
    fn ndp_is_answered_with_valid_checksum_and_bounded_replies() {
        let mut net = Network::new(US);
        let mut data = vec![0; 64];
        data[0] = 0x60;
        data[5] = 24;
        data[6] = 58;
        data[7] = 255;
        data[8..24].copy_from_slice(&PEER.link_local().octets());
        data[24..40].copy_from_slice(&US.link_local().octets());
        data[40] = 135;
        data[48..64].copy_from_slice(&US.link_local().octets());
        let sum = checksum(&data);
        data[42..44].copy_from_slice(&sum.to_be_bytes());
        for _ in 0..32 {
            assert!(net
                .inbound(Ipv6Packet {
                    src: PEER,
                    dst: US,
                    data: data.clone()
                })
                .is_none());
        }
        assert_eq!(net.replies.len(), 16);
        let r = net.replies.pop_front().unwrap();
        assert_eq!(r.dst, PEER);
        assert_eq!(r.data[40], 136);
        assert_eq!(checksum(&r.data), 0);
        data[7] = 64;
        net.inbound(Ipv6Packet {
            src: PEER,
            dst: US,
            data,
        });
        assert_eq!(net.replies.len(), 15);
    }
}
