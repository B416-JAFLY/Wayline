use std::collections::HashMap;
use std::io;
use std::net::{Ipv6Addr, SocketAddr, SocketAddrV6, UdpSocket};
use std::time::{Duration, Instant};

use omdrop_net::dns::{
    self, Question, Record, CLASS_IN, CLASS_UNICAST_RESPONSE, TYPE_AAAA, TYPE_ANY, TYPE_PTR,
    TYPE_SRV, TYPE_TXT,
};
use socket2::{Domain, Protocol, Socket, Type};

const MDNS_PORT: u16 = 5353;
const SERVICE_TYPE: &str = "_airdrop._tcp.local";
const MDNS_GROUP: Ipv6Addr = Ipv6Addr::new(0xff02, 0, 0, 0, 0, 0, 0, 0xfb);

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NetworkInterface {
    pub name: String,
    pub index: u32,
    pub link_local: Ipv6Addr,
}

impl NetworkInterface {
    pub fn endpoint(&self, port: u16) -> SocketAddrV6 {
        SocketAddrV6::new(self.link_local, port, 0, self.index)
    }

    pub fn multicast_endpoint(&self) -> SocketAddrV6 {
        SocketAddrV6::new(MDNS_GROUP, MDNS_PORT, 0, self.index)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AirDropService {
    pub instance: String,
    pub host: String,
    pub port: u16,
    pub address: SocketAddrV6,
    pub flags: Option<u32>,
}

pub struct MdnsResponder {
    interface: NetworkInterface,
    instance: String,
    host: String,
    port: u16,
    flags: u32,
    socket: UdpSocket,
}

impl MdnsResponder {
    pub fn bind(
        interface: NetworkInterface,
        service_id: &str,
        port: u16,
        flags: u32,
    ) -> io::Result<Self> {
        let socket = make_socket(&interface, MDNS_PORT, true)?;
        Ok(Self {
            instance: format!("{service_id}.{SERVICE_TYPE}"),
            host: format!("omdrop-{service_id}.local"),
            interface,
            port,
            flags,
            socket,
        })
    }

    pub fn instance(&self) -> &str {
        &self.instance
    }
    pub fn host(&self) -> &str {
        &self.host
    }

    pub fn announce_once(&self) -> io::Result<()> {
        let packet = dns::response(&self.records(false)).map_err(invalid_data)?;
        self.socket
            .send_to(&packet, self.interface.multicast_endpoint())?;
        Ok(())
    }

    pub fn goodbye_once(&self) -> io::Result<()> {
        let packet = dns::response(&self.records(true)).map_err(invalid_data)?;
        self.socket
            .send_to(&packet, self.interface.multicast_endpoint())?;
        Ok(())
    }

    /// Withdraw earlier instances belonging to this receiver on the same socket.
    /// Never include the active instance in a retirement response.
    pub fn retire_services_once(&self, service_ids: &[String]) -> io::Result<()> {
        self.send_retirements(service_ids, self.interface.multicast_endpoint().into())
    }

    fn send_retirements(&self, service_ids: &[String], destination: SocketAddr) -> io::Result<()> {
        // Each instance is four records. Keep each datagram below the IPv6
        // minimum MTU; combining the full history requires fragmentation.
        for records in self.retirement_records(service_ids).chunks(4) {
            let packet = dns::response(records).map_err(invalid_data)?;
            self.socket.send_to(&packet, destination)?;
        }
        Ok(())
    }

    fn retirement_records(&self, service_ids: &[String]) -> Vec<Record> {
        let mut records = Vec::new();
        for id in service_ids.iter().take(8) {
            if id.len() != 12 || !id.bytes().all(|b| b.is_ascii_hexdigit()) {
                continue;
            }
            let instance = format!("{id}.{SERVICE_TYPE}");
            if instance.eq_ignore_ascii_case(&self.instance) {
                continue;
            }
            records.extend(records_for(
                &self.interface,
                &instance,
                &format!("omdrop-{id}.local"),
                self.port,
                self.flags,
                true,
            ));
        }
        records
    }

    /// Handle one pending mDNS query. Returns `false` when no datagram was ready.
    pub fn poll_query(&self) -> io::Result<bool> {
        self.poll_query_retiring(&[])
    }

    /// Also withdraw known old instances when a peer returns after the initial
    /// cleanup burst. Only our own saved history may be supplied here.
    pub fn poll_query_retiring(&self, retired_service_ids: &[String]) -> io::Result<bool> {
        let mut buffer = [0u8; 9000];
        let (length, peer) = match self.socket.recv_from(&mut buffer) {
            Ok(value) => value,
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => return Ok(false),
            Err(error) => return Err(error),
        };
        let packet = &buffer[..length];
        let parsed = dns::parse_record_views(packet).map_err(invalid_data)?;
        if packet.get(2).is_some_and(|flags| flags & 0x80 != 0) {
            return Ok(true);
        }
        let mut wants_ptr = false;
        let mut wants_srv = false;
        let mut wants_txt = false;
        let mut wants_aaaa = false;
        let mut wants_retirement = false;
        let mut unicast = false;
        for question in parsed.0 {
            let name = question.name.to_ascii_lowercase();
            let any = question.kind == TYPE_ANY;
            if name == SERVICE_TYPE && (question.kind == TYPE_PTR || any) {
                wants_ptr = true;
            } else if name == self.instance.to_ascii_lowercase() {
                wants_srv |= question.kind == TYPE_SRV || any;
                wants_txt |= question.kind == TYPE_TXT || any;
            } else if name == self.host.to_ascii_lowercase() && (question.kind == TYPE_AAAA || any)
            {
                wants_aaaa = true;
            }
            wants_retirement |= retired_service_ids.iter().any(|id| {
                name.eq_ignore_ascii_case(&format!("{id}.{SERVICE_TYPE}"))
                    || name.eq_ignore_ascii_case(&format!("omdrop-{id}.local"))
            });
            unicast |= question.class & CLASS_UNICAST_RESPONSE != 0;
        }
        let wants_active = wants_ptr || wants_srv || wants_txt || wants_aaaa;
        if !(wants_active || wants_retirement) {
            return Ok(true);
        }
        let destination = if unicast || peer.port() != MDNS_PORT {
            peer
        } else {
            SocketAddr::V6(self.interface.multicast_endpoint())
        };
        if wants_ptr || wants_retirement {
            self.send_retirements(retired_service_ids, destination)?;
        }
        if wants_active {
            let packet = dns::response(&self.records(false)).map_err(invalid_data)?;
            self.socket.send_to(&packet, destination)?;
        }
        Ok(true)
    }

    fn records(&self, goodbye: bool) -> Vec<Record> {
        let ttl = if goodbye { 0 } else { 120 };
        let long_ttl = if goodbye { 0 } else { 4500 };
        let mut srv = Vec::new();
        srv.extend_from_slice(&0u16.to_be_bytes());
        srv.extend_from_slice(&0u16.to_be_bytes());
        srv.extend_from_slice(&self.port.to_be_bytes());
        srv.extend_from_slice(&dns::encode_name(&self.host).unwrap_or_default());
        let txt = format!("flags={}", self.flags);
        vec![
            Record {
                name: SERVICE_TYPE.into(),
                kind: TYPE_PTR,
                class: CLASS_IN,
                ttl: long_ttl,
                data: dns::encode_name(&self.instance).unwrap_or_default(),
            },
            Record {
                name: self.instance.clone(),
                kind: TYPE_SRV,
                class: CLASS_IN | CLASS_UNICAST_RESPONSE,
                ttl,
                data: srv,
            },
            Record {
                name: self.instance.clone(),
                kind: TYPE_TXT,
                class: CLASS_IN | CLASS_UNICAST_RESPONSE,
                ttl: long_ttl,
                data: [vec![txt.len() as u8], txt.into_bytes()].concat(),
            },
            Record {
                name: self.host.clone(),
                kind: TYPE_AAAA,
                class: CLASS_IN | CLASS_UNICAST_RESPONSE,
                ttl,
                data: self.interface.link_local.octets().to_vec(),
            },
        ]
    }
}

pub struct MdnsBrowser {
    interface: NetworkInterface,
    own_address: Option<Ipv6Addr>,
    socket: UdpSocket,
    cache: ServiceCache,
}

impl MdnsBrowser {
    pub fn bind(interface: NetworkInterface, own_address: Option<Ipv6Addr>) -> io::Result<Self> {
        let socket = make_socket(&interface, MDNS_PORT, true)?;
        Ok(Self {
            interface,
            own_address,
            socket,
            cache: ServiceCache::default(),
        })
    }

    pub fn query(&self) -> io::Result<()> {
        let questions = self.cache.questions(Instant::now());
        // Bound each query even when many services are present.
        for batch in questions.chunks(16) {
            let packet = dns::query(batch).map_err(invalid_data)?;
            self.socket
                .send_to(&packet, self.interface.multicast_endpoint())?;
        }
        Ok(())
    }

    /// Drain pending datagrams and return the current, unexpired service snapshot.
    /// Late TXT records update the snapshot instead of being suppressed forever.
    pub fn recv(&mut self) -> io::Result<Vec<AirDropService>> {
        let mut buffer = [0u8; 9000];
        for _ in 0..64 {
            let length = match self.socket.recv(&mut buffer) {
                Ok(length) => length,
                Err(error)
                    if matches!(
                        error.kind(),
                        io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut
                    ) =>
                {
                    break
                }
                Err(error) => return Err(error),
            };
            // Ignore malformed multicast packets from unrelated applications.
            let _ = self.cache.absorb(&buffer[..length], Instant::now());
        }
        let now = Instant::now();
        self.cache.prune(now);
        Ok(self.cache.services(&self.interface, self.own_address, now))
    }
}

#[derive(Default)]
struct ServiceCache {
    instances: HashMap<String, Instant>,
    srv: HashMap<String, (String, u16, Instant)>,
    txt: HashMap<String, (Option<u32>, Instant)>,
    addresses: HashMap<String, (Ipv6Addr, Instant)>,
}

impl ServiceCache {
    fn prune(&mut self, now: Instant) {
        self.instances.retain(|_, until| *until > now);
        self.srv.retain(|_, (_, _, until)| *until > now);
        self.txt.retain(|_, (_, until)| *until > now);
        self.addresses.retain(|_, (_, until)| *until > now);
    }

    fn questions(&self, now: Instant) -> Vec<Question> {
        let mut questions = vec![Question {
            name: SERVICE_TYPE.into(),
            kind: TYPE_PTR,
            class: CLASS_IN,
        }];
        for (instance, until) in &self.instances {
            if *until <= now {
                continue;
            }
            if !self
                .srv
                .get(instance)
                .is_some_and(|(_, _, until)| *until > now)
            {
                questions.push(Question {
                    name: instance.clone(),
                    kind: TYPE_SRV,
                    class: CLASS_IN,
                });
            }
            if !self
                .txt
                .get(instance)
                .is_some_and(|(_, until)| *until > now)
            {
                questions.push(Question {
                    name: instance.clone(),
                    kind: TYPE_TXT,
                    class: CLASS_IN,
                });
            }
        }
        for (host, _, until) in self.srv.values() {
            if *until > now
                && !self
                    .addresses
                    .get(host)
                    .is_some_and(|(_, until)| *until > now)
            {
                questions.push(Question {
                    name: host.clone(),
                    kind: TYPE_AAAA,
                    class: CLASS_IN,
                });
            }
        }
        questions
    }

    fn absorb(&mut self, packet: &[u8], now: Instant) -> io::Result<()> {
        let (_, records) = dns::parse_record_views(packet).map_err(invalid_data)?;
        if packet.get(2).is_none_or(|flags| flags & 0x80 == 0) {
            return Ok(());
        }
        for record in records {
            if record.class & 0x7fff != CLASS_IN {
                continue;
            }
            let name = record.name.to_ascii_lowercase();
            let until = now + Duration::from_secs(u64::from(record.ttl));
            let is_service = name.ends_with(&format!(".{SERVICE_TYPE}"));
            match record.kind {
                TYPE_PTR if name == SERVICE_TYPE => {
                    let mut at = record.data_offset;
                    let instance = dns::read_name(packet, &mut at)
                        .map_err(invalid_data)?
                        .to_ascii_lowercase();
                    if !instance.ends_with(&format!(".{SERVICE_TYPE}")) {
                        continue;
                    }
                    if record.ttl == 0 {
                        self.instances.remove(&instance);
                        self.srv.remove(&instance);
                        self.txt.remove(&instance);
                    } else {
                        self.instances.insert(instance, until);
                    }
                }
                TYPE_SRV if is_service && record.data_len >= 7 => {
                    if record.ttl == 0 {
                        self.srv.remove(&name);
                        continue;
                    }
                    let data = &packet[record.data_offset..record.data_offset + record.data_len];
                    let port = u16::from_be_bytes([data[4], data[5]]);
                    let mut at = record.data_offset + 6;
                    let host = dns::read_name(packet, &mut at)
                        .map_err(invalid_data)?
                        .to_ascii_lowercase();
                    self.instances.insert(name.clone(), until);
                    self.srv.insert(name, (host, port, until));
                }
                TYPE_TXT if is_service => {
                    if record.ttl == 0 {
                        self.txt.remove(&name);
                        continue;
                    }
                    let data = &packet[record.data_offset..record.data_offset + record.data_len];
                    let flags = txt_entries(data).find_map(|entry| {
                        entry
                            .strip_prefix("flags=")
                            .and_then(|value| value.parse().ok())
                    });
                    self.txt.insert(name, (flags, until));
                }
                TYPE_AAAA if record.data_len == 16 => {
                    let address = Ipv6Addr::from(
                        <[u8; 16]>::try_from(&packet[record.data_offset..record.data_offset + 16])
                            .unwrap(),
                    );
                    if record.ttl == 0 {
                        if self
                            .addresses
                            .get(&name)
                            .is_some_and(|(known, _)| *known == address)
                        {
                            self.addresses.remove(&name);
                        }
                    } else if address.is_unicast_link_local() {
                        self.addresses.insert(name, (address, until));
                    }
                }
                _ => {}
            }
        }
        Ok(())
    }

    fn services(
        &self,
        interface: &NetworkInterface,
        own: Option<Ipv6Addr>,
        now: Instant,
    ) -> Vec<AirDropService> {
        let mut result = Vec::new();
        for (instance, (host, port, until)) in &self.srv {
            if *until <= now || *port == 0 {
                continue;
            }
            let Some((address, address_until)) = self.addresses.get(host) else {
                continue;
            };
            if *address_until <= now || own == Some(*address) {
                continue;
            }
            let flags = self
                .txt
                .get(instance)
                .filter(|(_, until)| *until > now)
                .and_then(|(flags, _)| *flags);
            result.push(AirDropService {
                instance: instance.clone(),
                host: host.clone(),
                port: *port,
                address: SocketAddrV6::new(*address, *port, 0, interface.index),
                flags,
            });
        }
        result.sort_by(|a, b| a.instance.cmp(&b.instance));
        result
    }
}

fn txt_entries(data: &[u8]) -> impl Iterator<Item = &str> {
    let mut entries = Vec::new();
    let mut offset = 0;
    while offset < data.len() {
        let length = usize::from(data[offset]);
        offset += 1;
        if offset + length > data.len() {
            break;
        }
        if let Ok(entry) = std::str::from_utf8(&data[offset..offset + length]) {
            entries.push(entry);
        }
        offset += length;
    }
    entries.into_iter()
}

fn make_socket(interface: &NetworkInterface, port: u16, reuse: bool) -> io::Result<UdpSocket> {
    let socket = Socket::new(Domain::IPV6, Type::DGRAM, Some(Protocol::UDP))?;
    socket.set_only_v6(true)?;
    if reuse {
        socket.set_reuse_address(true)?;
    }
    socket.set_multicast_if_v6(interface.index)?;
    socket.set_multicast_hops_v6(255)?;
    socket.set_multicast_loop_v6(true)?;
    socket.bind(&SocketAddrV6::new(Ipv6Addr::UNSPECIFIED, port, 0, 0).into())?;
    socket.join_multicast_v6(&MDNS_GROUP, interface.index)?;
    let socket: UdpSocket = socket.into();
    if port == MDNS_PORT {
        socket.set_nonblocking(true)?;
    }
    Ok(socket)
}

fn invalid_data(error: impl std::fmt::Display) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, error.to_string())
}

fn records_for(
    interface: &NetworkInterface,
    instance: &str,
    host: &str,
    port: u16,
    flags: u32,
    goodbye: bool,
) -> Vec<Record> {
    let ttl = if goodbye { 0 } else { 120 };
    let long_ttl = if goodbye { 0 } else { 4500 };
    let mut srv = Vec::new();
    srv.extend_from_slice(&0u16.to_be_bytes());
    srv.extend_from_slice(&0u16.to_be_bytes());
    srv.extend_from_slice(&port.to_be_bytes());
    srv.extend_from_slice(&dns::encode_name(host).unwrap());
    let txt = format!("flags={flags}");
    vec![
        Record {
            name: SERVICE_TYPE.into(),
            kind: TYPE_PTR,
            class: CLASS_IN,
            ttl: long_ttl,
            data: dns::encode_name(instance).unwrap(),
        },
        Record {
            name: instance.into(),
            kind: TYPE_SRV,
            class: CLASS_IN | CLASS_UNICAST_RESPONSE,
            ttl,
            data: srv,
        },
        Record {
            name: instance.into(),
            kind: TYPE_TXT,
            class: CLASS_IN | CLASS_UNICAST_RESPONSE,
            ttl: long_ttl,
            data: [vec![txt.len() as u8], txt.into_bytes()].concat(),
        },
        Record {
            name: host.into(),
            kind: TYPE_AAAA,
            class: CLASS_IN | CLASS_UNICAST_RESPONSE,
            ttl,
            data: interface.link_local.octets().to_vec(),
        },
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    fn interface() -> NetworkInterface {
        NetworkInterface {
            name: "test".into(),
            index: 1,
            link_local: "fe80::1234".parse().unwrap(),
        }
    }

    #[test]
    fn responder_emits_airdrop_records() {
        let interface = interface();
        let packet = dns::response(&records_for(
            &interface,
            "abcdef123456._airdrop._tcp.local",
            "omdrop-abcdef123456.local",
            8771,
            137,
            false,
        ))
        .unwrap();
        let parsed = dns::parse(&packet).unwrap();
        assert_eq!(parsed.answers.len(), 4);
        assert!(parsed.answers.iter().any(|record| record.kind == TYPE_PTR));
    }

    #[test]
    fn browser_absorbs_response_records() {
        let interface_value = interface();
        let packet = dns::response(&records_for(
            &interface_value,
            "abcdef123456._airdrop._tcp.local",
            "omdrop-abcdef123456.local",
            8771,
            137,
            false,
        ))
        .unwrap();
        let mut cache = ServiceCache::default();
        let now = Instant::now();
        cache.absorb(&packet, now).unwrap();
        let found = cache.services(&interface_value, Some("fe80::ffff".parse().unwrap()), now);
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].port, 8771);
        assert_eq!(found[0].flags, Some(137));
    }

    #[test]
    fn retiring_old_names_removes_cached_entries_without_withdrawing_active_name() {
        let iface = interface();
        let active = "222222222222";
        let old = "111111111111";
        let responder = MdnsResponder {
            interface: iface.clone(),
            instance: format!("{active}.{SERVICE_TYPE}"),
            host: format!("omdrop-{active}.local"),
            port: 8771,
            flags: 137,
            socket: UdpSocket::bind("[::1]:0").unwrap(),
        };
        let mut cache = ServiceCache::default();
        let now = Instant::now();
        for id in [old, active] {
            let records = records_for(
                &iface,
                &format!("{id}.{SERVICE_TYPE}"),
                &format!("omdrop-{id}.local"),
                8771,
                137,
                false,
            );
            cache
                .absorb(&dns::response(&records).unwrap(), now)
                .unwrap();
        }
        assert_eq!(cache.services(&iface, None, now).len(), 2);
        let retired = responder.retirement_records(&[old.into(), active.into(), "invalid".into()]);
        assert_eq!(retired.len(), 4);
        assert!(retired.iter().all(|record| record.ttl == 0));
        cache
            .absorb(&dns::response(&retired).unwrap(), now)
            .unwrap();
        let remaining = cache.services(&iface, None, now);
        assert_eq!(remaining.len(), 1);
        assert_eq!(remaining[0].instance, responder.instance);
    }

    #[test]
    fn returning_peer_gets_all_retirements_in_mtu_sized_packets() {
        let iface = interface();
        let responder = MdnsResponder {
            interface: iface.clone(),
            instance: format!("abcdef123456.{SERVICE_TYPE}"),
            host: "omdrop-abcdef123456.local".into(),
            port: 8771,
            flags: 137,
            socket: UdpSocket::bind("[::1]:0").unwrap(),
        };
        responder
            .socket
            .set_read_timeout(Some(Duration::from_secs(2)))
            .unwrap();
        let peer = UdpSocket::bind("[::1]:0").unwrap();
        peer.set_read_timeout(Some(Duration::from_secs(2))).unwrap();
        let retired: Vec<_> = (0..8).map(|n| format!("{n:012x}")).collect();
        let mut cache = ServiceCache::default();
        let now = Instant::now();
        for id in &retired {
            let records = records_for(
                &iface,
                &format!("{id}.{SERVICE_TYPE}"),
                &format!("omdrop-{id}.local"),
                8771,
                137,
                false,
            );
            cache
                .absorb(&dns::response(&records).unwrap(), now)
                .unwrap();
        }
        let query = dns::query(&[Question {
            name: SERVICE_TYPE.into(),
            kind: TYPE_PTR,
            class: CLASS_IN,
        }])
        .unwrap();
        peer.send_to(&query, responder.socket.local_addr().unwrap())
            .unwrap();
        assert!(responder.poll_query_retiring(&retired).unwrap());
        let mut buffer = [0u8; 9000];
        for _ in 0..9 {
            let n = peer.recv(&mut buffer).unwrap();
            assert!(n <= 1232, "mDNS must fit the IPv6 minimum MTU");
            cache.absorb(&buffer[..n], now).unwrap();
        }
        let remaining = cache.services(&iface, None, now);
        assert_eq!(remaining.len(), 1);
        assert_eq!(remaining[0].instance, responder.instance);

        // A stale host lookup also receives withdrawals, without reannouncing
        // the old instance or waiting for another startup/reconfiguration.
        let query = dns::query(&[Question {
            name: format!("omdrop-{}.local", retired[0]),
            kind: TYPE_AAAA,
            class: CLASS_IN | CLASS_UNICAST_RESPONSE,
        }])
        .unwrap();
        peer.send_to(&query, responder.socket.local_addr().unwrap())
            .unwrap();
        assert!(responder.poll_query_retiring(&retired).unwrap());
        for _ in &retired {
            let n = peer.recv(&mut buffer).unwrap();
            assert!(dns::parse(&buffer[..n])
                .unwrap()
                .answers
                .iter()
                .all(|r| r.ttl == 0));
        }
    }

    #[test]
    fn split_case_insensitive_records_refresh_flags_and_expire() {
        let iface = interface();
        let now = Instant::now();
        let records = records_for(
            &iface,
            "PHONE._airdrop._tcp.local",
            "PHONE.local",
            8770,
            0x1b3fb,
            false,
        );
        let mut cache = ServiceCache::default();
        let without_txt: Vec<_> = records
            .iter()
            .filter(|r| r.kind != TYPE_TXT)
            .cloned()
            .collect();
        cache
            .absorb(&dns::response(&without_txt).unwrap(), now)
            .unwrap();
        assert_eq!(cache.services(&iface, None, now)[0].flags, None);
        assert!(cache.questions(now).iter().any(|q| q.kind == TYPE_TXT));
        let mut txt = records[2].clone();
        txt.name = txt.name.to_lowercase();
        cache.absorb(&dns::response(&[txt]).unwrap(), now).unwrap();
        assert_eq!(cache.services(&iface, None, now)[0].flags, Some(0x1b3fb));
        assert!(!cache.questions(now).iter().any(|q| q.kind == TYPE_TXT));
        assert!(cache
            .services(&iface, None, now + Duration::from_secs(121))
            .is_empty());
        cache
            .absorb(&dns::response(&records).unwrap(), now)
            .unwrap();
        let goodbye: Vec<_> = records
            .into_iter()
            .map(|mut record| {
                record.ttl = 0;
                record
            })
            .collect();
        cache
            .absorb(&dns::response(&goodbye).unwrap(), now)
            .unwrap();
        assert!(cache.services(&iface, None, now).is_empty());
    }

    #[test]
    fn ptr_query_response_is_not_a_zero_ttl_goodbye() {
        let responder = MdnsResponder {
            interface: interface(),
            instance: "phone._airdrop._tcp.local".into(),
            host: "phone.local".into(),
            port: 8771,
            flags: 137,
            socket: UdpSocket::bind("[::1]:0").unwrap(),
        };
        responder
            .socket
            .set_read_timeout(Some(Duration::from_secs(2)))
            .unwrap();
        let peer = UdpSocket::bind("[::1]:0").unwrap();
        peer.set_read_timeout(Some(Duration::from_secs(2))).unwrap();
        let query = dns::query(&[Question {
            name: SERVICE_TYPE.into(),
            kind: TYPE_PTR,
            class: CLASS_IN | CLASS_UNICAST_RESPONSE,
        }])
        .unwrap();
        peer.send_to(&query, responder.socket.local_addr().unwrap())
            .unwrap();
        assert!(responder.poll_query().unwrap());
        let mut buffer = [0u8; 4096];
        let n = peer.recv(&mut buffer).unwrap();
        let response = dns::parse(&buffer[..n]).unwrap();
        assert_eq!(response.answers.len(), 4);
        assert!(response.answers.iter().all(|record| record.ttl > 0));
    }
}
