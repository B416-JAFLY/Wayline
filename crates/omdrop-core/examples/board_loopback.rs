//! Hardware integration test. Local TLS file transfer only; never contacts a phone.
//! Run with OWL + new firmware active. Both screen lanes are queried from the actual MCU.
use omdrop_board::{query, Phase};
use omdrop_core::*;
use std::{
    fs,
    net::{SocketAddr, TcpListener},
    sync::Arc,
    thread,
    time::Duration,
};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    enable_board_feedback();
    if std::env::args().any(|arg| arg == "--failure-cases") {
        return failure_cases();
    }
    fs::create_dir_all("build/cardputer-ui")?;
    fs::write(
        "build/cardputer-ui/label.bin",
        omdrop_board::raster_label("本机回环测试 · Windows")?,
    )?;
    let temp = tempfile::tempdir()?;
    let source = temp.path().join("双向状态测试.bin");
    let mut bytes = vec![0u8; 8 * 1024 * 1024];
    getrandom::fill(&mut bytes)?;
    fs::write(&source, &bytes)?;
    let incoming = temp.path().join("received");
    let identity = Identity::load_or_create(temp.path().join("identity"), "Loopback test")?;
    let tls = server_config(&identity)?;
    let core = Arc::new(ReceiverCore::new(
        ReceiverOptions::new("本机回环测试", "Windows", &incoming).with_accept_all(),
    ));
    let listener = TcpListener::bind("[::1]:0")?;
    let SocketAddr::V6(address) = listener.local_addr()? else {
        unreachable!()
    };
    let server = thread::spawn(move || {
        let (stream, _) = listener.accept().unwrap();
        stream
            .set_read_timeout(Some(Duration::from_secs(30)))
            .unwrap();
        ReceiverCore::serve_tls_connection(stream, tls, core).unwrap()
    });
    let selected = NearbyReceiver {
        name: "本机回环测试".into(),
        service: AirDropService {
            instance: "test._airdrop._tcp.local".into(),
            host: "localhost".into(),
            port: address.port(),
            address,
            flags: Some(137),
        },
    };
    let mut both_seen = false;
    let mut last_observed = 0;
    let report = send_to_receiver(
        &selected,
        &identity,
        &[source],
        &SendOptions {
            sender_name: "本机回环测试".into(),
            sender_model: "Windows".into(),
            target_name: selected.name.clone(),
            timeout: Duration::from_secs(30),
            format: SendFormat::Gzip,
            ble_wake: false,
            legacy_ble: false,
        },
        |event| match event {
            SendEvent::Preparing
            | SendEvent::ModeSelected(_)
            | SendEvent::WaitingForAcceptance { .. }
            | SendEvent::WaitingForConfirmation => {
                println!("{event:?}");
                thread::sleep(Duration::from_millis(700));
            }
            SendEvent::Uploading { sent, total } => {
                // Slow this real file transfer so both lanes and intermediate progress can be observed.
                thread::sleep(Duration::from_millis(160));
                if sent > last_observed + 1024 * 1024 && sent < total {
                    last_observed = sent;
                    let tx = query(0).expect("query ESP32 send state");
                    let rx = query(1).expect("query ESP32 receive state");
                    println!(
                        "MCU tx={:?} {} / {}; rx={:?} {} / {:?}",
                        tx.phase,
                        tx.done,
                        tx.total,
                        rx.phase,
                        rx.done,
                        rx.known_total.then_some(rx.total)
                    );
                    assert_eq!(tx.phase, Phase::Transferring);
                    assert_eq!(rx.phase, Phase::Transferring);
                    assert!(tx.done > 0 && rx.done > 0 && !rx.known_total);
                    both_seen = true;
                }
            }
            SendEvent::Completed(_) => println!("HTTP success confirmed"),
        },
    )?;
    let events = server.join().unwrap();
    assert!(events
        .iter()
        .any(|e| matches!(e, ReceiverEvent::Received { .. })));
    assert_eq!(fs::read(incoming.join("双向状态测试.bin"))?, bytes);
    assert!(both_seen);
    thread::sleep(Duration::from_millis(700));
    for direction in 0..=1 {
        let value = query(direction)?;
        assert_eq!(value.phase, Phase::Complete);
        assert_eq!(value.done, report.archive_bytes);
        println!(
            "MCU direction={direction}: {:?}, {} bytes; exact file comparison passed",
            value.phase, value.done
        );
    }
    thread::sleep(Duration::from_secs(11));
    assert_eq!(query(0)?.phase, Phase::Idle);
    assert_eq!(query(1)?.phase, Phase::Idle);
    println!("PASS: real TLS transfer, send/receive progress, final confirmation, exact bytes and idle return");
    Ok(())
}

fn failure_cases() -> Result<(), Box<dyn std::error::Error>> {
    for decline in [true, false] {
        let temp = tempfile::tempdir()?;
        let file = temp.path().join("test.bin");
        let mut bytes = vec![0; 1024 * 1024];
        getrandom::fill(&mut bytes)?;
        fs::write(&file, bytes)?;
        let identity = Identity::load_or_create(temp.path().join("identity"), "Loopback test")?;
        let tls = server_config(&identity)?;
        let mut options =
            ReceiverOptions::new("本机回环测试", "Windows", temp.path().join("received"));
        options.accept_all = !decline;
        let core = Arc::new(ReceiverCore::new(options));
        let listener = TcpListener::bind("[::1]:0")?;
        let SocketAddr::V6(address) = listener.local_addr()? else {
            unreachable!()
        };
        let server = thread::spawn(move || {
            let (stream, _) = listener.accept().unwrap();
            stream
                .set_read_timeout(Some(Duration::from_secs(5)))
                .unwrap();
            ReceiverCore::serve_tls_connection(stream, tls, core)
        });
        let selected = NearbyReceiver {
            name: "本机回环测试".into(),
            service: AirDropService {
                instance: "test._airdrop._tcp.local".into(),
                host: "localhost".into(),
                port: address.port(),
                address,
                flags: None,
            },
        };
        let cancel = SendCancellation::default();
        let result = send_to_receiver_cancellable(
            &selected,
            &identity,
            &[file],
            &SendOptions {
                sender_name: "本机回环测试".into(),
                sender_model: "Windows".into(),
                target_name: selected.name.clone(),
                timeout: Duration::from_secs(5),
                format: SendFormat::Gzip,
                ble_wake: false,
                legacy_ble: false,
            },
            &cancel,
            |event| {
                if !decline && matches!(event, SendEvent::Uploading { sent, .. } if sent > 0) {
                    thread::sleep(Duration::from_millis(600));
                    cancel.cancel();
                }
            },
        );
        assert!(result.is_err());
        let received = server.join().unwrap();
        if decline {
            assert!(received.is_ok());
        } else {
            assert!(received.is_err());
        }
        thread::sleep(Duration::from_millis(700));
        let tx = query(0)?;
        let rx = query(1)?;
        assert_eq!(
            tx.phase,
            if decline {
                Phase::Failed
            } else {
                Phase::Cancelled
            }
        );
        assert_eq!(rx.phase, Phase::Failed);
        if decline {
            assert_eq!(tx.reason, omdrop_board::Reason::Declined);
            assert_eq!(rx.reason, omdrop_board::Reason::Declined);
        }
        println!(
            "PASS: {}: MCU tx={:?}/{:?}, rx={:?}/{:?}",
            if decline {
                "remote declined"
            } else {
                "cancelled during upload"
            },
            tx.phase,
            tx.reason,
            rx.phase,
            rx.reason
        );
    }
    Ok(())
}
