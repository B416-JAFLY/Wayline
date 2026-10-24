//! Wintun 0.14.1 ABI, loaded only from the executable's directory.
use crate::{driver::Netif, network::Network, Ipv6Packet, MacAddr, MTU};
use std::{
    io, mem,
    os::windows::ffi::OsStrExt,
    path::Path,
    ptr,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc, Mutex,
    },
    thread::{self, JoinHandle},
};
use tokio::sync::mpsc;
use windows_sys::{
    core::GUID,
    Win32::{
        Foundation::{
            FreeLibrary, GetLastError, ERROR_BUFFER_OVERFLOW, ERROR_NO_MORE_ITEMS,
            ERROR_OBJECT_ALREADY_EXISTS, HANDLE, HMODULE, WAIT_FAILED,
        },
        NetworkManagement::{IpHelper::*, Ndis::NET_LUID_LH},
        Networking::WinSock::{IpDadStatePreferred, RouterDiscoveryDisabled, AF_INET6},
        System::{
            LibraryLoader::{
                GetProcAddress, LoadLibraryExW, LOAD_LIBRARY_SEARCH_DLL_LOAD_DIR,
                LOAD_LIBRARY_SEARCH_SYSTEM32,
            },
            Threading::WaitForSingleObject,
        },
    },
};

type Create = unsafe extern "system" fn(*const u16, *const u16, *const GUID) -> HANDLE;
type Close = unsafe extern "system" fn(HANDLE);
type Luid = unsafe extern "system" fn(HANDLE, *mut NET_LUID_LH);
type Start = unsafe extern "system" fn(HANDLE, u32) -> HANDLE;
type Event = unsafe extern "system" fn(HANDLE) -> HANDLE;
type Receive = unsafe extern "system" fn(HANDLE, *mut u32) -> *mut u8;
type Release = unsafe extern "system" fn(HANDLE, *const u8);
type Allocate = unsafe extern "system" fn(HANDLE, u32) -> *mut u8;
type SendPacket = unsafe extern "system" fn(HANDLE, *const u8);

struct Library(HMODULE);
impl Drop for Library {
    fn drop(&mut self) {
        // SAFETY: this is our LoadLibraryExW reference, after sessions have ended.
        unsafe {
            FreeLibrary(self.0);
        }
    }
}

struct Session {
    _library: Library,
    adapter: HANDLE,
    session: HANDLE,
    close: Close,
    end: Close,
    event: Event,
    receive: Receive,
    release: Release,
    allocate: Allocate,
    send: SendPacket,
}
// SAFETY: Wintun's packet/session APIs are thread safe. Arc keeps the DLL,
// session and adapter alive until the reader joins and all packet calls end.
unsafe impl Send for Session {}
// SAFETY: receive/release have one reader; allocate/send have one writer.
// Destruction occurs only after the final Arc is released.
unsafe impl Sync for Session {}
impl Drop for Session {
    fn drop(&mut self) {
        // SAFETY: no worker or borrowed packet remains when the final Arc is dropped.
        unsafe {
            if !self.session.is_null() {
                (self.end)(self.session);
            }
            if !self.adapter.is_null() {
                (self.close)(self.adapter);
            }
        }
    }
}

impl Session {
    fn open(dll: &Path, mac: MacAddr) -> io::Result<Self> {
        if !dll.is_absolute() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "Wintun path must be absolute",
            ));
        }
        let path: Vec<u16> = dll.as_os_str().encode_wide().chain(Some(0)).collect();
        // SAFETY: path is terminated and lives through the synchronous call.
        let library = Library(unsafe {
            LoadLibraryExW(
                path.as_ptr(),
                ptr::null_mut(),
                LOAD_LIBRARY_SEARCH_DLL_LOAD_DIR | LOAD_LIBRARY_SEARCH_SYSTEM32,
            )
        });
        if library.0.is_null() {
            return Err(context("load wintun.dll", io::Error::last_os_error()));
        }
        macro_rules! function {
            ($name:literal,$ty:ty) => {{
                // SAFETY: the fixed name and function type match Wintun 0.14.1's
                // wintun.h ABI. The Library stays alive for every call.
                let raw = unsafe { GetProcAddress(library.0, concat!($name, "\0").as_ptr()) }
                    .ok_or_else(|| context($name, io::Error::last_os_error()))?;
                unsafe { mem::transmute::<unsafe extern "system" fn() -> isize, $ty>(raw) }
            }};
        }
        let create = function!("WintunCreateAdapter", Create);
        let luid = function!("WintunGetAdapterLUID", Luid);
        let start = function!("WintunStartSession", Start);
        let close = function!("WintunCloseAdapter", Close);
        let end = function!("WintunEndSession", Close);
        let event = function!("WintunGetReadWaitEvent", Event);
        let receive = function!("WintunReceivePacket", Receive);
        let release = function!("WintunReleaseReceivePacket", Release);
        let allocate = function!("WintunAllocateSendPacket", Allocate);
        let send = function!("WintunSendPacket", SendPacket);
        let mut s = Session {
            _library: library,
            adapter: ptr::null_mut(),
            session: ptr::null_mut(),
            close,
            end,
            event,
            receive,
            release,
            allocate,
            send,
        };
        let name: Vec<u16> = "awdl0\0".encode_utf16().collect();
        let kind: Vec<u16> = "OWL\0".encode_utf16().collect();
        let guid = GUID {
            data1: 0x6f776c30,
            data2: 0x6177,
            data3: 0x646c,
            data4: [0x8a, 0x5e, 0x30, 0x77, 0x69, 0x6e, 0x30, 0x31],
        };
        // SAFETY: all buffers and output rows have the Win32 ABI and live through
        // these synchronous calls. Session owns handles as soon as they are set.
        unsafe {
            s.adapter = create(name.as_ptr(), kind.as_ptr(), &guid);
            if s.adapter.is_null() {
                return Err(context("create awdl0", io::Error::last_os_error()));
            }
            let mut id: NET_LUID_LH = mem::zeroed();
            luid(s.adapter, &mut id);
            configure(id, mac)?;
            s.session = start(s.adapter, 0x400000);
            if s.session.is_null() {
                return Err(context("start Wintun session", io::Error::last_os_error()));
            }
        }
        Ok(s)
    }
}

fn context(action: &str, e: io::Error) -> io::Error {
    io::Error::new(e.kind(), format!("{action}: {e}"))
}
fn status(code: u32, action: &str) -> io::Result<()> {
    if code == 0 {
        Ok(())
    } else {
        Err(context(action, io::Error::from_raw_os_error(code as i32)))
    }
}

fn configure(id: NET_LUID_LH, mac: MacAddr) -> io::Result<()> {
    // SAFETY: Win32 initializes the C rows; unions are accessed using the IPv6
    // discriminator. The system table remains alive until FreeMibTable.
    unsafe {
        let mut row: MIB_IPINTERFACE_ROW = mem::zeroed();
        InitializeIpInterfaceEntry(&mut row);
        row.Family = AF_INET6;
        row.InterfaceLuid = id;
        status(GetIpInterfaceEntry(&mut row), "read awdl0 IPv6 settings")?;
        row.NlMtu = MTU;
        row.DadTransmits = 0;
        row.RouterDiscoveryBehavior = RouterDiscoveryDisabled;
        row.SitePrefixLength = 0;
        status(SetIpInterfaceEntry(&mut row), "configure awdl0 IPv6")?;
        let mut table: *mut MIB_UNICASTIPADDRESS_TABLE = ptr::null_mut();
        status(
            GetUnicastIpAddressTable(AF_INET6, &mut table),
            "list IPv6 addresses",
        )?;
        struct Table(*mut MIB_UNICASTIPADDRESS_TABLE);
        impl Drop for Table {
            fn drop(&mut self) {
                unsafe {
                    FreeMibTable(self.0.cast());
                }
            }
        }
        let table = Table(table);
        let rows =
            std::slice::from_raw_parts((*table.0).Table.as_ptr(), (*table.0).NumEntries as usize);
        for a in rows {
            if a.InterfaceLuid.Value == id.Value
                && a.Address.Ipv6.sin6_addr.u.Byte != mac.link_local().octets()
            {
                status(
                    DeleteUnicastIpAddressEntry(a),
                    "remove generated awdl0 address",
                )?;
            }
        }
        let mut addr: MIB_UNICASTIPADDRESS_ROW = mem::zeroed();
        InitializeUnicastIpAddressEntry(&mut addr);
        addr.InterfaceLuid = id;
        addr.Address.Ipv6.sin6_family = AF_INET6;
        addr.Address.Ipv6.sin6_addr.u.Byte = mac.link_local().octets();
        addr.OnLinkPrefixLength = 64;
        addr.DadState = IpDadStatePreferred;
        let result = CreateUnicastIpAddressEntry(&addr);
        if result != ERROR_OBJECT_ALREADY_EXISTS {
            status(result, "assign AWDL address")?;
        }
    }
    Ok(())
}

pub struct Wintun {
    session: Arc<Session>,
    stopped: Arc<AtomicBool>,
    error: Arc<Mutex<Option<String>>>,
    packets: mpsc::Receiver<Vec<u8>>,
    reader: Option<JoinHandle<()>>,
    network: Network,
}

impl Wintun {
    pub fn open(dll: &Path, mac: MacAddr) -> io::Result<Self> {
        let session = Arc::new(Session::open(dll, mac)?);
        let stopped = Arc::new(AtomicBool::new(false));
        let error = Arc::new(Mutex::new(None));
        let (tx, packets) = mpsc::channel(128);
        let s = session.clone();
        let stop = stopped.clone();
        let failure = error.clone();
        let reader = thread::Builder::new()
            .name("wayline-wintun".into())
            .spawn(move || {
                let result = (|| -> io::Result<()> {
                    // SAFETY: s keeps this event's owner alive. It is not closed separately.
                    let event = unsafe { (s.event)(s.session) };
                    while !stop.load(Ordering::Acquire) {
                        let mut len = 0;
                        // SAFETY: returned packet remains borrowed until release below.
                        let p = unsafe { (s.receive)(s.session, &mut len) };
                        if p.is_null() {
                            let code = unsafe { GetLastError() };
                            if code != ERROR_NO_MORE_ITEMS {
                                return Err(io::Error::from_raw_os_error(code as i32));
                            }
                            if unsafe { WaitForSingleObject(event, 100) } == WAIT_FAILED {
                                return Err(io::Error::last_os_error());
                            }
                            continue;
                        }
                        let bytes = if (40..=MTU).contains(&len) {
                            // SAFETY: Wintun guarantees exactly len bytes until release.
                            Some(unsafe { std::slice::from_raw_parts(p, len as usize) }.to_vec())
                        } else {
                            None
                        };
                        unsafe {
                            (s.release)(s.session, p);
                        }
                        if let Some(bytes) = bytes {
                            if tx.blocking_send(bytes).is_err() {
                                break;
                            }
                        }
                    }
                    Ok(())
                })();
                if let Err(e) = result {
                    *failure.lock().unwrap() = Some(e.to_string());
                }
            })?;
        Ok(Self {
            session,
            stopped,
            error,
            packets,
            reader: Some(reader),
            network: Network::new(mac),
        })
    }
}

impl Netif for Wintun {
    async fn recv(&mut self) -> io::Result<Ipv6Packet> {
        loop {
            if let Some(e) = self.error.lock().unwrap().as_ref() {
                return Err(io::Error::other(e.clone()));
            }
            if let Some(reply) = self.network.replies.pop_front() {
                return Ok(reply);
            }
            let bytes = self.packets.recv().await.ok_or_else(|| {
                io::Error::other(
                    self.error
                        .lock()
                        .unwrap()
                        .as_deref()
                        .unwrap_or("Wintun reader stopped"),
                )
            })?;
            if let Some(p) = self.network.outbound(&bytes) {
                return Ok(p);
            }
        }
    }
    fn deliver(&mut self, packet: Ipv6Packet) {
        let Some(data) = self.network.inbound(packet) else {
            return;
        };
        // SAFETY: Session stays alive, and data fits the validated MTU. A successful
        // allocation is filled exactly and handed back before any other operation.
        unsafe {
            let p = (self.session.allocate)(self.session.session, data.len() as u32);
            if p.is_null() {
                let code = GetLastError();
                if code != ERROR_BUFFER_OVERFLOW {
                    *self.error.lock().unwrap() =
                        Some(io::Error::from_raw_os_error(code as i32).to_string());
                }
                return;
            }
            ptr::copy_nonoverlapping(data.as_ptr(), p, data.len());
            (self.session.send)(self.session.session, p);
        }
    }
    fn peer_up(&mut self, mac: MacAddr) {
        self.network.peer_up(mac);
    }
    fn peer_down(&mut self, mac: MacAddr) {
        self.network.peer_down(mac);
    }
}
impl Drop for Wintun {
    fn drop(&mut self) {
        self.stopped.store(true, Ordering::Release);
        self.packets.close(); // Unblocks a reader waiting on the bounded channel.
        if let Some(reader) = self.reader.take() {
            let _ = reader.join();
        }
    }
}
