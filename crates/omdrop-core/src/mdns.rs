//! Windows UDP sockets around Owly's mDNS codec, responder and browser.
use owly_airdrop::proto::mdns::{self, BrowseEvent, Browser, Message, Service};
use socket2::{Domain, Protocol, Socket, Type};
use std::{
    collections::BTreeMap,
    io,
    net::{Ipv6Addr, SocketAddr, SocketAddrV6, UdpSocket},
    time::Instant,
};
const MDNS_PORT: u16 = mdns::MDNS_PORT;
const MDNS_GROUP: Ipv6Addr = mdns::MDNS_GROUP;
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
    service: Service,
    socket: UdpSocket,
}
impl MdnsResponder {
    pub fn bind(
        interface: NetworkInterface,
        service_id: &str,
        port: u16,
        flags: u32,
    ) -> io::Result<Self> {
        if service_id.is_empty()
            || service_id.len() > 56
            || !service_id
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'-')
        {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "invalid mDNS service id",
            ));
        }
        let socket = make_socket(&interface)?;
        let service = Service {
            instance: service_id.into(),
            host: format!("omdrop-{service_id}"),
            port,
            addr: interface.link_local,
            flags,
        };
        Ok(Self {
            interface,
            service,
            socket,
        })
    }
    pub fn announce_once(&self) -> io::Result<()> {
        self.send(
            &self.service.announcement(),
            self.interface.multicast_endpoint().into(),
        )
    }
    pub fn goodbye_once(&self) -> io::Result<()> {
        self.send(
            &self.service.goodbye(),
            self.interface.multicast_endpoint().into(),
        )
    }
    fn send(&self, bytes: &[u8], to: SocketAddr) -> io::Result<()> {
        self.socket.send_to(bytes, to).map(|_| ())
    }
    fn retired(&self, ids: &[String]) -> Vec<Service> {
        ids.iter()
            .filter(|id| **id != self.service.instance)
            .take(32)
            .map(|id| Service {
                instance: id.clone(),
                host: format!("omdrop-{id}"),
                ..self.service.clone()
            })
            .collect()
    }
    pub fn retire_services_once(&self, ids: &[String]) -> io::Result<()> {
        for old in self.retired(ids) {
            self.send(&old.goodbye(), self.interface.multicast_endpoint().into())?;
        }
        Ok(())
    }
    pub fn poll_query_retiring(&self, ids: &[String]) -> io::Result<()> {
        let mut buf = [0; mdns::MAX_MESSAGE];
        for _ in 0..64 {
            let (n, from) = match self.socket.recv_from(&mut buf) {
                Ok(v) => v,
                Err(e) if e.kind() == io::ErrorKind::WouldBlock => break,
                Err(e) => return Err(e),
            };
            if !on_interface(from, self.interface.index) {
                continue;
            }
            let Ok(query) = Message::decode(&buf[..n]) else {
                continue;
            };
            if query.response {
                continue;
            }
            if let Some(reply) = self.service.answer(&query, from.port() != MDNS_PORT) {
                let to = if reply.unicast {
                    from
                } else {
                    self.interface.multicast_endpoint().into()
                };
                self.send(&reply.message, to)?;
            }
            for old in self.retired(ids) {
                if old.answer(&query, from.port() != MDNS_PORT).is_some() {
                    self.send(&old.goodbye(), self.interface.multicast_endpoint().into())?;
                }
            }
        }
        Ok(())
    }
}
pub struct MdnsBrowser {
    interface: NetworkInterface,
    socket: UdpSocket,
    browser: Browser,
    services: BTreeMap<String, AirDropService>,
    ignore: Option<Ipv6Addr>,
}
impl MdnsBrowser {
    pub fn bind(interface: NetworkInterface, ignore: Option<Ipv6Addr>) -> io::Result<Self> {
        Ok(Self {
            socket: make_socket(&interface)?,
            interface,
            browser: Browser::new(Instant::now()),
            services: BTreeMap::new(),
            ignore,
        })
    }
    pub fn query(&mut self) -> io::Result<()> {
        if let Some(bytes) = self.browser.on_timer(Instant::now()) {
            self.socket
                .send_to(&bytes, self.interface.multicast_endpoint())?;
        }
        self.drain();
        Ok(())
    }
    fn drain(&mut self) {
        while let Some(event) = self.browser.poll_event() {
            match event {
                BrowseEvent::Found(found) if Some(found.addr) != self.ignore => {
                    let instance = format!("{}._airdrop._tcp.local", found.instance);
                    self.services.insert(
                        found.instance,
                        AirDropService {
                            instance,
                            host: format!("{}.local", found.host),
                            port: found.port,
                            address: SocketAddrV6::new(
                                found.addr,
                                found.port,
                                0,
                                self.interface.index,
                            ),
                            flags: Some(found.flags),
                        },
                    );
                }
                BrowseEvent::Found(found) => {
                    self.services.remove(&found.instance);
                }
                BrowseEvent::Lost(id) => {
                    self.services.remove(&id);
                }
            }
        }
    }
    pub fn recv(&mut self) -> io::Result<Vec<AirDropService>> {
        self.query()?;
        let mut buf = [0; mdns::MAX_MESSAGE];
        for _ in 0..64 {
            let (n, from) = match self.socket.recv_from(&mut buf) {
                Ok(v) => v,
                Err(e) if e.kind() == io::ErrorKind::WouldBlock => break,
                Err(e) => return Err(e),
            };
            if !on_interface(from, self.interface.index) {
                continue;
            }
            if let Ok(msg) = Message::decode(&buf[..n]) {
                self.browser.handle(&msg, Instant::now());
            }
        }
        self.drain();
        Ok(self.services.values().cloned().collect())
    }
}
fn on_interface(from: SocketAddr, index: u32) -> bool {
    matches!(from,SocketAddr::V6(addr) if addr.ip().is_unicast_link_local()&&(addr.scope_id()==0||addr.scope_id()==index))
}
fn make_socket(interface: &NetworkInterface) -> io::Result<UdpSocket> {
    let socket = Socket::new(Domain::IPV6, Type::DGRAM, Some(Protocol::UDP))?;
    socket.set_only_v6(true)?;
    socket.set_reuse_address(true)?;
    socket.set_multicast_if_v6(interface.index)?;
    socket.set_multicast_hops_v6(255)?;
    socket.set_multicast_loop_v6(true)?;
    socket.bind(&SocketAddrV6::new(Ipv6Addr::UNSPECIFIED, MDNS_PORT, 0, 0).into())?;
    socket.join_multicast_v6(&MDNS_GROUP, interface.index)?;
    socket.set_nonblocking(true)?;
    Ok(socket.into())
}
