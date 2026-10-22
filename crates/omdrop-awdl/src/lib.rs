//! AWDL on the host, with the ESP32 as a raw-frame radio.

mod network;
pub mod serial;
#[cfg(windows)]
pub mod windows;
pub mod wire;
pub use owly_awdl::{driver, Ipv6Packet, MacAddr};

use owly_awdl::{Channel, Config, Presence, RadioConstraints, TxRate};
use std::io;

pub const UPSTREAM_REVISION: &str = "2e9b2b0228432d5e402a9c90a031b419960d0bd1";
pub const MTU: u32 = 1450;

pub fn configuration(mac: [u8; 6], name: &str, target: &str, channel: u8) -> io::Result<Config> {
    if mac == [0; 6] || mac[0] & 1 != 0 {
        return Err(invalid("radio returned an invalid unicast MAC"));
    }
    if !matches!(
        (target, channel),
        ("esp32s3", 6) | ("esp32c5", 6 | 44 | 149)
    ) {
        return Err(invalid(format!(
            "unsupported radio/channel: {target}/{channel}"
        )));
    }
    if name.is_empty() || name.len() > 63 || name.chars().any(char::is_control) {
        return Err(invalid(
            "AWDL name must be 1..63 UTF-8 bytes without control characters",
        ));
    }
    let mut config = Config::new(
        MacAddr(mac),
        name,
        RadioConstraints {
            channel: Channel(channel),
            presence: Presence::Fixed,
        },
    );
    config.max_frame = omdrop_proto::PROTO_MAX_FRAME;
    config.rate = TxRate::BASIC;
    // The ESP32 wire protocol does not expose a no-ACK transmission flag.
    config.no_ack = false;
    // Block Ack is a firmware capability; never impersonate it from the host.
    config.accept_addba = false;
    config.amsdu = false;
    Ok(config)
}

pub(crate) fn invalid(message: impl Into<String>) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message.into())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn device_capabilities_match_the_firmware() {
        let mac = [2, 0, 0, 0, 0, 1];
        for (target, channel) in [
            ("esp32s3", 6),
            ("esp32c5", 6),
            ("esp32c5", 44),
            ("esp32c5", 149),
        ] {
            let c = configuration(mac, "Omdrop", target, channel).unwrap();
            assert_eq!(c.max_frame, 1600);
            assert!(!c.no_ack && !c.accept_addba && !c.amsdu);
            assert_eq!(c.radio.presence, Presence::Fixed);
        }
        for (target, channel) in [("esp32s3", 44), ("esp32c6", 149), ("esp32c5", 1)] {
            assert!(configuration(mac, "Omdrop", target, channel).is_err());
        }
        assert!(configuration([0; 6], "Omdrop", "esp32s3", 6).is_err());
    }
}
