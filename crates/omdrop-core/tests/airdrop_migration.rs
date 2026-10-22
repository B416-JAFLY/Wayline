//! Real Windows-compatible TLS and protocol regressions; no radio or service.
use omdrop_core::*;
use owly_airdrop::{
    driver::sender::prepare,
    proto::{
        archive::compress::Compressor,
        http,
        receiver::{Config, Connection, Event},
        sender::{self, ResponseReader},
    },
};
use std::{
    fs,
    io::{self, Read, Write},
    net::{SocketAddr, TcpListener, TcpStream},
    sync::{
        atomic::{AtomicBool, Ordering},
        mpsc, Arc,
    },
    thread,
    time::Duration,
};

fn options(format: SendFormat) -> SendOptions {
    SendOptions {
        sender_name: "Windows 测试".into(),
        sender_model: "Windows".into(),
        target_name: "receiver".into(),
        timeout: Duration::from_secs(5),
        format,
        ble_wake: false,
        legacy_ble: false,
    }
}
fn identity(dir: &std::path::Path) -> Identity {
    Identity::load_or_create(dir.join("id"), "Omdrop").unwrap()
}
fn selected(addr: std::net::SocketAddrV6) -> NearbyReceiver {
    NearbyReceiver {
        name: "receiver".into(),
        service: AirDropService {
            instance: "receiver._airdrop._tcp.local".into(),
            host: "localhost".into(),
            port: addr.port(),
            address: addr,
            flags: Some(2),
        },
    }
}
fn start(
    core: Arc<ReceiverCore>,
    id: &Identity,
) -> (
    NearbyReceiver,
    thread::JoinHandle<Result<Vec<ReceiverEvent>, omdrop_core::ReceiverError>>,
) {
    let listener = TcpListener::bind("[::1]:0").unwrap();
    let SocketAddr::V6(addr) = listener.local_addr().unwrap() else {
        panic!()
    };
    let config = server_config(id).unwrap();
    let worker = thread::spawn(move || {
        let (s, _) = listener.accept().unwrap();
        ReceiverCore::serve_tls_connection(s, config, core)
    });
    (selected(addr), worker)
}

#[test]
fn tls_round_trips_both_compressions_empty_unicode_and_directory_files() {
    for format in [SendFormat::Gzip, SendFormat::DvZip] {
        let root = tempfile::tempdir().unwrap();
        let id = identity(root.path());
        let folder = root.path().join("文档");
        fs::create_dir(&folder).unwrap();
        let mut data = vec![0; 2 * 1024 * 1024];
        getrandom::fill(&mut data).unwrap();
        fs::write(folder.join("中文.bin"), &data).unwrap();
        fs::write(folder.join("empty"), []).unwrap();
        let dest = root.path().join("received");
        let core = Arc::new(ReceiverCore::new(
            ReceiverOptions::new("receiver", "Windows", &dest).with_accept_all(),
        ));
        let (peer, worker) = start(core.clone(), &id);
        let mut events = Vec::new();
        let report =
            send_to_receiver(&peer, &id, &[folder], &options(format), |e| events.push(e)).unwrap();
        let received = worker.join().unwrap().unwrap();
        assert_eq!(fs::read(dest.join("文档/中文.bin")).unwrap(), data);
        assert_eq!(fs::metadata(dest.join("文档/empty")).unwrap().len(), 0);
        assert_eq!(report.source_bytes, data.len() as u64);
        assert!(matches!(events.last(), Some(SendEvent::Completed(_))));
        assert!(received
            .iter()
            .any(|e| matches!(e,ReceiverEvent::Receiving{received,..} if *received>0)));
        assert!(matches!(
            received.last(),
            Some(ReceiverEvent::Received { .. })
        ));
        assert!(!core.has_active_transfers());
    }
}

#[test]
fn rejection_and_upload_cancellation_leave_no_published_files() {
    for decline in [true, false] {
        let root = tempfile::tempdir().unwrap();
        let id = identity(root.path());
        let file = root.path().join("data.bin");
        let mut data = vec![0; 4 * 1024 * 1024];
        getrandom::fill(&mut data).unwrap();
        fs::write(&file, data).unwrap();
        let dest = root.path().join("received");
        let mut opts = ReceiverOptions::new("receiver", "Windows", &dest);
        opts.accept_all = !decline;
        let core = Arc::new(ReceiverCore::new(opts));
        let (peer, worker) = start(core.clone(), &id);
        let cancel = SendCancellation::default();
        let result = send_to_receiver_cancellable(
            &peer,
            &id,
            &[file],
            &options(SendFormat::DvZip),
            &cancel,
            |e| {
                if !decline && matches!(e,SendEvent::Uploading{sent,..} if sent>0) {
                    cancel.cancel();
                }
            },
        );
        assert_eq!(
            result.unwrap_err().kind(),
            if decline {
                TransferFailureKind::Declined
            } else {
                TransferFailureKind::Cancelled
            }
        );
        let _ = worker.join().unwrap();
        assert!(!core.has_active_transfers());
        assert!(!dest.exists() || fs::read_dir(dest).unwrap().next().is_none());
    }
}

fn protocol_upload(kind: Compressor, chunked: bool) -> (tempfile::TempDir, Vec<u8>, Vec<u8>) {
    let root = tempfile::tempdir().unwrap();
    let file = root.path().join("a.txt");
    fs::write(&file, b"verified").unwrap();
    let (offer, mut archive, len) = prepare(&[file], kind).unwrap();
    let mut body = Vec::new();
    archive.read_to_end(&mut body).unwrap();
    let cfg = sender::Config::new("sender", "abcdef123456");
    let ask = sender::ask_request(&cfg, "TEST-ID", &offer);
    let length = len.to_string();
    let mut headers = vec![
        ("Content-Type", kind.content_type()),
        ("TransferID", "TEST-ID"),
    ];
    if chunked {
        headers.push(("Transfer-Encoding", "chunked"));
    } else {
        headers.push(("Content-Length", &length));
    }
    let head = http::post_head("/Upload", &headers);
    let mut wire = head;
    wire.extend(if chunked { http::chunk(&body) } else { body });
    (root, ask, wire)
}
fn ask(connection: &mut Connection, bytes: &[u8]) {
    connection.receive(bytes);
    let mut asked = false;
    while let Some(e) = connection.poll() {
        if let Event::Ask(_) = e {
            asked = true;
            connection.accept();
        }
    }
    assert!(asked);
}
fn drain(connection: &mut Connection) -> Vec<Event> {
    let mut out = Vec::new();
    while let Some(e) = connection.poll() {
        out.push(e);
    }
    out
}

#[test]
fn strict_upload_waits_for_http_completion_and_checks_compression_checksum() {
    for kind in [Compressor::Gzip, Compressor::DvZip] {
        let (_root, offer, mut wire) = protocol_upload(kind, true);
        let mut c = Connection::new(Arc::new(Config::new("receiver")));
        ask(&mut c, &offer);
        c.receive(&wire);
        assert!(!drain(&mut c)
            .iter()
            .any(|e| matches!(e, Event::UploadComplete)));
        assert!(!c.finish_validated_dvzip_boundary());
        c.receive(b"0\r\n\r\n");
        assert!(drain(&mut c)
            .iter()
            .any(|e| matches!(e, Event::UploadComplete)));
        // A valid archive does not excuse a malformed HTTP trailer.
        let mut c = Connection::new(Arc::new(Config::new("receiver")));
        ask(&mut c, &offer);
        wire.extend(b"0\r\nbad header\r\n\r\n");
        c.receive(&wire);
        let events = drain(&mut c);
        assert!(events.iter().any(|e| matches!(e, Event::UploadFailed(_))));
        assert!(!events.iter().any(|e| matches!(e, Event::UploadComplete)));
    }
    let (_root, offer, mut wire) = protocol_upload(Compressor::Gzip, false);
    let pos = wire.len() - 8;
    wire[pos] ^= 0xff;
    let mut c = Connection::new(Arc::new(Config::new("receiver")));
    ask(&mut c, &offer);
    c.receive(&wire);
    let events = drain(&mut c);
    assert!(events.iter().any(|e| matches!(e, Event::UploadFailed(_))));
    assert!(!events.iter().any(|e| matches!(e, Event::UploadComplete)));
}

#[test]
fn dvzip_boundary_requires_explicit_policy_complete_chunk_and_no_tail() {
    let (_root, offer, wire) = protocol_upload(Compressor::DvZip, true);
    for tail in [b"".as_slice(), b"1", b"1\r\nx"] {
        let mut cfg = Config::new("receiver");
        cfg.allow_dvzip_archive_boundary = true;
        let mut c = Connection::new(Arc::new(cfg));
        ask(&mut c, &offer);
        c.receive(&wire);
        drain(&mut c);
        c.receive(tail);
        drain(&mut c);
        assert_eq!(c.finish_validated_dvzip_boundary(), tail.is_empty());
    }
    let mut cfg = Config::new("receiver");
    cfg.allow_dvzip_archive_boundary = true;
    let mut c = Connection::new(Arc::new(cfg));
    ask(&mut c, &offer);
    c.receive(&wire[..wire.len() - 1]);
    drain(&mut c);
    assert!(!c.finish_validated_dvzip_boundary());
}

#[test]
fn truncated_upload_and_unauthorized_upload_never_complete() {
    let (_root, offer, wire) = protocol_upload(Compressor::DvZip, false);
    let mut c = Connection::new(Arc::new(Config::new("receiver")));
    c.receive(&wire);
    assert!(!drain(&mut c)
        .iter()
        .any(|e| matches!(e, Event::UploadStarted | Event::UploadComplete)));
    let mut c = Connection::new(Arc::new(Config::new("receiver")));
    ask(&mut c, &offer);
    c.receive(&wire[..wire.len() - 3]);
    drain(&mut c);
    c.receive_eof();
    assert!(drain(&mut c)
        .iter()
        .any(|e| matches!(e, Event::UploadFailed(_))));
}

#[test]
fn a_complete_empty_archive_does_not_satisfy_an_offered_file() {
    use owly_airdrop::proto::archive::{compress::Encoder, cpio};
    let (_root, offer, _) = protocol_upload(Compressor::Gzip, false);
    let mut encoder = Encoder::new(Compressor::Gzip, flate2::Compression::fast());
    let mut body = encoder.write(&cpio::odc_trailer());
    body.extend(encoder.finish());
    let len = body.len().to_string();
    let mut wire = http::post_head(
        "/Upload",
        &[
            ("Content-Type", "application/x-cpio"),
            ("TransferID", "TEST-ID"),
            ("Content-Length", &len),
        ],
    );
    wire.extend(body);
    let mut c = Connection::new(Arc::new(Config::new("receiver")));
    ask(&mut c, &offer);
    c.receive(&wire);
    let events = drain(&mut c);
    assert!(events.iter().any(|e| matches!(e, Event::UploadFailed(_))));
    assert!(!events.iter().any(|e| matches!(e, Event::UploadComplete)));
}

#[test]
fn cancelled_archive_preparation_stops_between_blocks() {
    use std::cell::Cell;
    let root = tempfile::tempdir().unwrap();
    let file = root.path().join("large");
    fs::write(&file, vec![0; 4 * 1024 * 1024]).unwrap();
    let calls = Cell::new(0);
    let result =
        owly_airdrop::driver::sender::prepare_cancellable(&[file], Compressor::Gzip, &|| {
            calls.set(calls.get() + 1);
            if calls.get() > 8 {
                Err(io::Error::other("cancelled"))
            } else {
                Ok(())
            }
        });
    assert!(result.is_err());
    assert!(calls.get() < 15);
}

#[test]
fn stopping_listener_closes_and_joins_idle_tls_workers() {
    let root = tempfile::tempdir().unwrap();
    let id = identity(root.path());
    let cfg = server_config(&id).unwrap();
    let listener = TcpListener::bind("[::1]:0").unwrap();
    let addr = listener.local_addr().unwrap();
    let core = Arc::new(ReceiverCore::new(ReceiverOptions::new(
        "receiver",
        "Windows",
        root.path(),
    )));
    let shutdown = Arc::new(AtomicBool::new(false));
    let stop = shutdown.clone();
    let (tx, rx) = mpsc::channel();
    let worker =
        thread::spawn(move || ReceiverCore::run_tcp_listener(listener, cfg, core, stop, tx));
    assert!(matches!(
        rx.recv_timeout(Duration::from_secs(2)).unwrap(),
        ReceiverEvent::Listening
    ));
    let mut socket = TcpStream::connect(addr).unwrap();
    socket
        .set_read_timeout(Some(Duration::from_secs(2)))
        .unwrap();
    thread::sleep(Duration::from_millis(40));
    let start = std::time::Instant::now();
    shutdown.store(true, Ordering::Release);
    worker.join().unwrap().unwrap();
    assert!(start.elapsed() < Duration::from_secs(2));
    assert!(!matches!(socket.read(&mut [0]), Ok(1..)));
}

fn client(
    address: std::net::SocketAddrV6,
) -> rustls::StreamOwned<rustls::ClientConnection, TcpStream> {
    let socket = TcpStream::connect(address).unwrap();
    socket
        .set_read_timeout(Some(Duration::from_secs(3)))
        .unwrap();
    let cfg = owly_airdrop::driver::tls::client_config(None).unwrap();
    let connection =
        rustls::ClientConnection::new(cfg, owly_airdrop::driver::tls::server_name()).unwrap();
    rustls::StreamOwned::new(connection, socket)
}
fn response(stream: &mut impl Read) -> sender::Response {
    let mut reader = ResponseReader::new();
    let mut buf = [0; 4096];
    loop {
        let n = stream.read(&mut buf).unwrap();
        assert!(n > 0, "missing HTTP response");
        if let Some(reply) = reader.receive(&buf[..n]).unwrap() {
            return reply;
        }
    }
}

#[test]
fn tls_validated_dvzip_boundary_finishes_without_zero_chunk_or_eof() {
    let (_source, offer, wire) = protocol_upload(Compressor::DvZip, true);
    let root = tempfile::tempdir().unwrap();
    let id = identity(root.path());
    let dest = root.path().join("received");
    let mut opts = ReceiverOptions::new("receiver", "Windows", &dest).with_accept_all();
    opts.allow_dvzip_archive_boundary = true;
    let (peer, worker) = start(Arc::new(ReceiverCore::new(opts)), &id);
    let mut tls = client(peer.service.address);
    tls.write_all(&offer).unwrap();
    tls.flush().unwrap();
    assert_eq!(response(&mut tls).status, 200);
    tls.write_all(&wire).unwrap();
    tls.flush().unwrap();
    assert_eq!(response(&mut tls).status, 200);
    let events = worker.join().unwrap().unwrap();
    assert!(events
        .iter()
        .any(|e| matches!(e, ReceiverEvent::ArchiveBoundary { .. })));
    assert_eq!(fs::read(dest.join("a.txt")).unwrap(), b"verified");
}

#[test]
fn final_response_failure_rolls_back_committed_files() {
    let root = tempfile::tempdir().unwrap();
    let id = identity(root.path());
    let dest = root.path().join("received");
    let source = root.path().join("data");
    fs::write(&source, b"verified").unwrap();
    let core = Arc::new(ReceiverCore::new(
        ReceiverOptions::new("receiver", "Windows", &dest).with_accept_all(),
    ));
    let config = server_config(&id).unwrap();
    let listener = TcpListener::bind("[::1]:0").unwrap();
    let SocketAddr::V6(address) = listener.local_addr().unwrap() else {
        panic!()
    };
    let worker =
        thread::spawn(move || {
            let (socket, _) = listener.accept().unwrap();
            let write_end = socket.try_clone().unwrap();
            let mut completed = false;
            let result =
                ReceiverCore::serve_tls_connection_with_events(socket, config, core, |event| {
                    match event {
                        ReceiverEvent::Confirming { .. } => {
                            write_end.shutdown(std::net::Shutdown::Write).unwrap()
                        }
                        ReceiverEvent::Received { .. } => completed = true,
                        _ => {}
                    }
                });
            assert!(!completed);
            assert!(result.is_err());
        });
    assert!(send_to_receiver(
        &selected(address),
        &id,
        &[source],
        &options(SendFormat::Gzip),
        |_| {}
    )
    .is_err());
    worker.join().unwrap();
    assert!(fs::read_dir(dest).unwrap().next().is_none());
}

#[test]
fn request_paths_and_unoffered_files_are_rejected_by_the_protocol() {
    let root = tempfile::tempdir().unwrap();
    let file = root.path().join("allowed");
    fs::write(&file, b"a").unwrap();
    let (offer, _, _) = prepare(&[file], Compressor::Gzip).unwrap();
    let mut value = match offer {
        owly_airdrop::proto::messages::Offer::Files(files) => files,
        _ => panic!(),
    };
    value[0].bom_path = "../escape".into();
    let offer = owly_airdrop::proto::messages::Offer::Files(value);
    let cfg = sender::Config::new("sender", "123456abcdef");
    let bytes = sender::ask_request(&cfg, "id", &offer);
    let mut c = Connection::new(Arc::new(Config::new("receiver")));
    c.receive(&bytes);
    assert!(!drain(&mut c).iter().any(|e| matches!(e, Event::Ask(_))));
}

fn read_request(stream: &mut impl Read) -> (http::RequestHead, Vec<u8>) {
    let mut pending = Vec::new();
    let mut buffer = [0; 4096];
    let (head, n) = loop {
        if let Some(head) = http::parse_request(&pending, &http::Limits::default()).unwrap() {
            break head;
        }
        let n = stream.read(&mut buffer).unwrap();
        assert!(n > 0);
        pending.extend_from_slice(&buffer[..n]);
    };
    pending.drain(..n);
    let mut decoder = http::BodyDecoder::new(head.body, 1 << 20, &http::Limits::default()).unwrap();
    let mut body = Vec::new();
    while !decoder.is_done() {
        let step = decoder.decode(&pending).unwrap();
        body.extend_from_slice(&pending[step.data]);
        pending.drain(..step.consumed);
        if step.consumed == 0 {
            let n = stream.read(&mut buffer).unwrap();
            assert!(n > 0);
            pending.extend_from_slice(&buffer[..n]);
        }
    }
    (head, body)
}

#[test]
fn sender_reconnects_after_acceptance_and_requires_successful_final_response() {
    for status in [200, 403, 0] {
        let root = tempfile::tempdir().unwrap();
        let id = identity(root.path());
        let file = root.path().join("a.txt");
        fs::write(&file, b"verified").unwrap();
        let cfg = server_config(&id).unwrap();
        let listener = TcpListener::bind("[::1]:0").unwrap();
        let SocketAddr::V6(addr) = listener.local_addr().unwrap() else {
            panic!()
        };
        let worker = thread::spawn(move || {
            let accept = || {
                let (s, _) = listener.accept().unwrap();
                s.set_read_timeout(Some(Duration::from_secs(3))).unwrap();
                rustls::StreamOwned::new(rustls::ServerConnection::new(cfg.clone()).unwrap(), s)
            };
            let mut first = accept();
            let (head, _) = read_request(&mut first);
            assert_eq!(head.target, "/Ask");
            first
                .write_all(&http::response(200, &[("Connection", "close")], b""))
                .unwrap();
            first.flush().unwrap();
            drop(first);
            let mut second = accept();
            let (head, body) = read_request(&mut second);
            assert_eq!(head.target, "/Upload");
            assert!(!body.is_empty());
            if status != 0 {
                second
                    .write_all(&http::response(status, &[("Connection", "close")], b""))
                    .unwrap();
                second.flush().unwrap();
            }
        });
        let mut completed = false;
        let result = send_to_receiver(
            &selected(addr),
            &id,
            &[file],
            &options(SendFormat::Gzip),
            |e| {
                if matches!(e, SendEvent::Completed(_)) {
                    completed = true;
                }
            },
        );
        worker.join().unwrap();
        assert_eq!(completed, status == 200);
        match status {
            200 => {
                result.unwrap();
            }
            403 => assert_eq!(result.unwrap_err().kind(), TransferFailureKind::Declined),
            _ => assert_eq!(result.unwrap_err().kind(), TransferFailureKind::Connection),
        }
    }
}
